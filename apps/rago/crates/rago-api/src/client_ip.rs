//! The customer's public IP on browser-facing requests (customer portal,
//! hosted-checkout returns).
//!
//! Iyzico requires a buyer IP on every payment. Charges started by the
//! customer carry their live IP; charges started by the clock (billing runs,
//! dunning) use the last IP captured here (see
//! `rago_jobs::collection::resolve_buyer_ip`).
//!
//! Resolution: the right-most `X-Forwarded-For` address, else `X-Real-IP`, else
//! the socket peer. Only public unicast addresses are kept: a loopback or
//! private address is the proxy, not the customer, and is never stored.
//!
//! Every rago host binds `127.0.0.1` only; Caddy is the sole public listener
//! (see `apps/DEPLOY-SHARED-PLANE.md`), so there is exactly one trusted proxy
//! hop. Caddy's `reverse_proxy` does not strip a client-supplied
//! `X-Forwarded-For` — it appends the real peer address to whatever the
//! client sent. That means the left-most entry is attacker-controlled (a
//! direct caller can send `X-Forwarded-For: 1.2.3.4` and have it accepted at
//! face value), while the right-most entry is always the one Caddy itself
//! observed and appended. Only that single trusted hop is read; earlier
//! entries are never consulted even if the last one fails to parse.

use std::net::{IpAddr, SocketAddr};

use axum::async_trait;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::HeaderMap;

/// The request's public client IP, when one could be determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub Option<IpAddr>);

impl ClientIp {
    pub fn as_string(&self) -> Option<String> {
        self.0.map(|ip| ip.to_string())
    }
}

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        Ok(ClientIp(client_ip(&parts.headers, peer)))
    }
}

/// Whether `ip` is a public unicast address (a real customer, not a proxy or
/// a private network hop).
pub fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                // 100.64.0.0/10 carrier-grade NAT
                || (o[0] == 100 && (o[1] & 0xc0) == 64))
        }
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(&IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00 // unique local
                || (seg0 & 0xffc0) == 0xfe80 // link local
                || seg0 == 0x2001 && v6.segments()[1] == 0x0db8) // documentation
        }
    }
}

fn parse_ip(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim().trim_matches('"');
    if let Ok(ip) = raw.parse::<IpAddr>() {
        return Some(ip);
    }
    // `1.2.3.4:5678` or `[2001:db8::1]:443`.
    raw.parse::<SocketAddr>().ok().map(|s| s.ip())
}

/// Resolve the public client IP from proxy headers, then the socket peer.
///
/// `X-Forwarded-For` is read from the **right**, trusting only the single
/// hop Caddy appends (see the module docs): a client-supplied left-most
/// entry is never used, even as a fallback if the last entry is absent or
/// unparsable, since any entry other than the last is attacker-controlled.
pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<IpAddr> {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .and_then(parse_ip)
        .filter(is_public);
    let real = || {
        headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_ip)
            .filter(is_public)
    };
    forwarded
        .or_else(real)
        .or_else(|| peer.map(|p| p.ip()).filter(is_public))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn right_most_forwarded_address_is_the_trusted_caddy_hop() {
        // Caddy appends the real peer address as the last entry; a customer
        // connecting directly through Caddy produces exactly this shape.
        let h = headers(&[("x-forwarded-for", "10.0.0.2, 85.105.12.7")]);
        assert_eq!(client_ip(&h, None), Some("85.105.12.7".parse().unwrap()));
    }

    #[test]
    fn left_most_client_supplied_forwarded_address_is_never_trusted() {
        // An attacker connecting directly to Caddy can send any left-most
        // value it likes; Caddy appends the real address (here `9.9.9.9`) as
        // the last entry. The forged `1.2.3.4` must never be returned.
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 9.9.9.9")]);
        assert_eq!(client_ip(&h, None), Some("9.9.9.9".parse().unwrap()));
        assert_ne!(client_ip(&h, None), Some("1.2.3.4".parse().unwrap()));
    }

    #[test]
    fn private_and_loopback_addresses_are_never_the_customer() {
        let h = headers(&[("x-forwarded-for", "10.1.2.3"), ("x-real-ip", "127.0.0.1")]);
        assert_eq!(client_ip(&h, Some("192.168.1.4:5000".parse().unwrap())), None);
        assert_eq!(client_ip(&HeaderMap::new(), Some("100.64.3.3:1".parse().unwrap())), None);
        assert_eq!(client_ip(&HeaderMap::new(), Some("[::1]:1".parse().unwrap())), None);
    }

    #[test]
    fn falls_back_to_real_ip_then_socket_peer() {
        let h = headers(&[("x-forwarded-for", "garbage"), ("x-real-ip", "2a02:4780::1")]);
        assert_eq!(client_ip(&h, None), Some("2a02:4780::1".parse().unwrap()));
        assert_eq!(
            client_ip(&HeaderMap::new(), Some("78.186.4.4:443".parse().unwrap())),
            Some("78.186.4.4".parse().unwrap())
        );
    }
}
