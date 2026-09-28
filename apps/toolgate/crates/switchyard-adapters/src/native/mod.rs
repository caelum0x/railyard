//! Native adapters, dispatched by the manifest's `native` key.

pub mod own;
pub mod public;
pub mod readable;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use switchyard_core::{Adapter, AdapterError, AdapterOutput, CallCtx, ToolManifest};

pub use public::PublicState;

/// Every native key this build can serve.
pub const KEYS: &[&str] = &[
    "relay.chat.complete",
    "scribase.db.query",
    "scribase.db.branch.create",
    "scribase.auth.magic_link.send",
    "rally.leads.search",
    "rally.company.enrich",
    "rally.email.verify",
    "rally.email.send",
    "menivor.video.render",
    "menivor.video.status",
    "menivor.tts.speak",
    "web.read.readable",
    "research.papers.arxiv",
    "dev.package.info.registries",
    "company.sec.filings",
    "domain.rdap.whois",
];

pub struct NativeAdapter {
    provider: String,
    tools: Vec<ToolManifest>,
    state: Arc<PublicState>,
}

impl NativeAdapter {
    pub fn new(provider: &str, tools: Vec<ToolManifest>, state: Arc<PublicState>) -> Self {
        NativeAdapter { provider: provider.into(), tools, state }
    }
}

#[async_trait]
impl Adapter for NativeAdapter {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn tools(&self) -> &[ToolManifest] {
        &self.tools
    }

    async fn call(&self, ctx: &CallCtx, tool: &str, input: Value) -> Result<AdapterOutput, AdapterError> {
        let m = self.tools.iter().find(|t| t.id == tool).ok_or_else(|| AdapterError::fatal(format!("{tool} not served here")))?;
        let key = m.native.as_deref().unwrap_or_default();
        let i = &input;
        match key {
            "relay.chat.complete" => own::relay_chat(ctx, i).await,
            "scribase.db.query" => own::scribase_query(ctx, i).await,
            "scribase.db.branch.create" => own::scribase_branch(ctx, i).await,
            "scribase.auth.magic_link.send" => own::scribase_magic_link(ctx, i).await,
            "rally.leads.search" => own::rally_leads(ctx, i).await,
            "rally.company.enrich" => own::rally_enrich(ctx, i).await,
            "rally.email.verify" => own::email_verify(ctx, i).await,
            "rally.email.send" => own::rally_send(ctx, i).await,
            "menivor.video.render" => own::menivor_render(ctx, i).await,
            "menivor.video.status" => own::menivor_status(ctx, i).await,
            "menivor.tts.speak" => own::menivor_tts(ctx, i).await,
            "web.read.readable" => readable::read(ctx, i).await,
            "research.papers.arxiv" => public::arxiv(ctx, i).await,
            "dev.package.info.registries" => public::package(ctx, i).await,
            "company.sec.filings" => public::sec_filings(&self.state, ctx, i).await,
            "domain.rdap.whois" => public::rdap(&self.state, ctx, i).await,
            other => Err(AdapterError::fatal(format!("unknown native handler {other}"))),
        }
    }
}
