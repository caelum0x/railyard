//! # switchyard-egress
//!
//! The only way Switchyard reaches the internet.
//!
//! - **Allowlist.** A host must be on the static allowlist (the union of
//!   provider `egress_hosts`, config-derived hosts and admin-registered BYO
//!   hosts) or on the request's own `extra_hosts`. `*.example.org` matches
//!   subdomains only.
//! - **Blocked addresses.** Every resolved address is checked; loopback,
//!   RFC1918, CGNAT, link-local, cloud metadata, multicast, documentation,
//!   reserved ranges and IPv6 forms that embed them (mapped, NAT64, 6to4) are
//!   refused. If *any* resolved address is blocked the request is refused.
//! - **Pinning.** The validated addresses are pinned and the connection uses
//!   exactly those, so a second DNS answer (rebinding) is never consulted.
//! - **Redirects.** At most 3, each hop re-validated; credential headers are
//!   dropped as soon as a hop leaves the original host.
//! - **Caps.** 10 s connect, per-request total timeout, bounded body size.
//! - Caller `Authorization`/`Cookie` headers are stripped; only injected
//!   credentials (`sensitive_headers`) are sent.

pub mod client;
pub mod policy;
pub mod resolver;

pub use client::{EgressConfig, SafeClient};
pub use policy::{host_allowed, is_blocked_ip};
pub use resolver::{Resolver, SystemResolver};
