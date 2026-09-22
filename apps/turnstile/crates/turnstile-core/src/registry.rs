//! Provider registry and discovery catalog.
//!
//! A provider usually gates more than one route or MCP tool. The registry is the
//! framework-agnostic catalog of the paid tools a Turnstile deployment exposes:
//! each [`ToolDescriptor`] names a resource, its price (integer microdollars,
//! never floats — ADR-0001), settlement scheme, and a human description. The
//! axum discovery endpoint ([`discover`](../../turnstile_axum/discovery/index.html))
//! serves the derived [`ToolCatalog`] so a calling agent can learn — before it
//! spends anything — exactly which resources are paid, how much, to which payee,
//! and under which scheme.
//!
//! The registry holds NO money and issues NO challenges; it is metadata only.
//! The live price a caller is actually charged is always the one the
//! [`Turnstile`](crate::Turnstile) gate quotes in its `402` challenge. Keeping a
//! single source of truth is the caller's job: build the registry from the same
//! [`TurnstileConfig`](crate::TurnstileConfig) the gate uses (see
//! [`ToolRegistry::from_config`] and [`ToolDescriptor::priced_by`]).

use aitp_client::codec::AITP_VERSION;
use aitp_client::{AmountMicros, Scheme};
use serde::Serialize;

use crate::config::TurnstileConfig;

/// One discoverable paid tool a provider exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolDescriptor {
    /// Stable, human-facing tool name (e.g. `"echo"`).
    pub name: String,
    /// The resource identity the gate challenges/validates against. For an HTTP
    /// route this is `"<METHOD> <path>"` (e.g. `"POST /v1/echo"`); for an MCP
    /// tool a stable id such as `"tools/call#echo"`.
    pub resource: String,
    /// One-line description of what the tool does.
    pub description: String,
    /// Price per call, in integer microdollars (never floats — ADR-0001).
    pub price_micros: AmountMicros,
    /// Settlement scheme (`aitp-hold` reserves-then-captures; `aitp-transfer`
    /// pays the exact price up front).
    pub scheme: Scheme,
}

impl ToolDescriptor {
    /// A tool priced explicitly.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        resource: impl Into<String>,
        description: impl Into<String>,
        price_micros: AmountMicros,
        scheme: Scheme,
    ) -> Self {
        Self {
            name: name.into(),
            resource: resource.into(),
            description: description.into(),
            price_micros,
            scheme,
        }
    }

    /// A tool priced by the live gate configuration, so the catalog can never
    /// drift from what the `402` challenge actually quotes. The scheme is always
    /// `aitp-hold` (Turnstile reserves the quote and captures actual usage).
    #[must_use]
    pub fn priced_by(
        name: impl Into<String>,
        resource: impl Into<String>,
        description: impl Into<String>,
        config: &TurnstileConfig,
    ) -> Self {
        Self::new(
            name,
            resource,
            description,
            config.price_micros,
            Scheme::Hold,
        )
    }
}

/// Serializable discovery view: the payee, currency, protocol version, and the
/// full set of priced tools. Served verbatim by the discovery endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCatalog {
    /// AITP protocol version this provider speaks.
    pub aitp_version: String,
    /// Provider receive address (`ait1…`) every challenge settles to.
    pub payee: String,
    /// Settlement currency (always `"AIT"` today).
    pub currency: String,
    /// Every discoverable paid tool.
    pub tools: Vec<ToolDescriptor>,
}

/// Immutable-by-construction catalog builder. Every mutator returns a NEW
/// registry rather than editing in place, so a shared base can be extended
/// without surprising another holder (coding-style: immutability).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRegistry {
    payee: String,
    currency: String,
    tools: Vec<ToolDescriptor>,
}

impl ToolRegistry {
    /// An empty registry for `payee` settling in `currency`.
    #[must_use]
    pub fn new(payee: impl Into<String>, currency: impl Into<String>) -> Self {
        Self {
            payee: payee.into(),
            currency: currency.into(),
            tools: Vec::new(),
        }
    }

    /// An empty registry whose payee/currency come from the live gate config.
    #[must_use]
    pub fn from_config(config: &TurnstileConfig) -> Self {
        Self::new(config.payee.clone(), config.currency.clone())
    }

    /// Return a NEW registry with `tool` added (or replaced, if a tool with the
    /// same `resource` already exists). Never mutates `self`.
    #[must_use]
    pub fn with_tool(&self, tool: ToolDescriptor) -> Self {
        let mut tools: Vec<ToolDescriptor> = self
            .tools
            .iter()
            .filter(|existing| existing.resource != tool.resource)
            .cloned()
            .collect();
        tools.push(tool);
        Self {
            payee: self.payee.clone(),
            currency: self.currency.clone(),
            tools,
        }
    }

    /// All registered tools.
    #[must_use]
    pub fn tools(&self) -> &[ToolDescriptor] {
        &self.tools
    }

    /// The descriptor for `resource`, if registered.
    #[must_use]
    pub fn tool_for(&self, resource: &str) -> Option<&ToolDescriptor> {
        self.tools.iter().find(|tool| tool.resource == resource)
    }

    /// The advertised price for `resource`, if registered. Advisory only — the
    /// gate's live challenge is the source of truth for what is actually charged.
    #[must_use]
    pub fn price_for(&self, resource: &str) -> Option<AmountMicros> {
        self.tool_for(resource).map(|tool| tool.price_micros)
    }

    /// Snapshot the registry as a serializable [`ToolCatalog`] for discovery.
    #[must_use]
    pub fn catalog(&self) -> ToolCatalog {
        ToolCatalog {
            aitp_version: AITP_VERSION.to_string(),
            payee: self.payee.clone(),
            currency: self.currency.clone(),
            tools: self.tools.clone(),
        }
    }
}

