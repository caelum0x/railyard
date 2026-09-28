//! Address and host policy.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn v4_blocked(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // 169.254/16 incl. 169.254.169.254 metadata
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || o[0] == 0 // 0.0.0.0/8
        || (o[0] == 100 && (o[1] & 0xC0) == 64) // 100.64/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0/24 IETF
        || (o[0] == 198 && (o[1] & 0xFE) == 18) // 198.18/15 benchmarking
        || o[0] >= 240 // 240/4 reserved
}

fn v6_blocked(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_blocked(v4);
    }
    // IPv4-compatible (deprecated ::a.b.c.d) and NAT64 64:ff9b::/96 embed an IPv4.
    if s[0..6] == [0, 0, 0, 0, 0, 0] || (s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0]) {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return ip.is_unspecified() || ip.is_loopback() || v4_blocked(v4);
    }
    // 6to4 2002::/16 embeds an IPv4 in segments 1-2.
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        if v4_blocked(v4) {
            return true;
        }
    }
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local (incl. fd00:ec2::254 metadata)
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x2001 && s[1] == 0) // Teredo
        || s[0] == 0x0100 && s[1..4] == [0, 0, 0] // 100::/64 discard
}

/// True when connecting to `ip` could reach something that is not the public internet.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => v6_blocked(v6),
    }
}

fn norm(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Exact match, or `*.suffix` matching a strict subdomain.
pub fn host_allowed<'a>(host: &str, allow: impl IntoIterator<Item = &'a String>) -> bool {
    let host = norm(host);
    allow.into_iter().any(|entry| {
        let entry = norm(entry);
        match entry.strip_prefix("*.") {
            Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
            None => host == entry,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(s: &str) -> bool {
        is_blocked_ip(s.parse().unwrap())
    }

    #[test]
    fn blocks_private_and_special_v4() {
        for ip in [
            "127.0.0.1", "127.8.9.10", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.1", "100.64.0.1", "100.127.255.254",
            "169.254.169.254", "169.254.1.1", "0.0.0.0", "0.1.2.3", "224.0.0.1", "255.255.255.255", "240.0.0.1", "192.0.0.170",
            "198.18.0.1", "192.0.2.1", "203.0.113.9",
        ] {
            assert!(blocked(ip), "{ip} must be blocked");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "93.184.216.34", "172.32.0.1", "100.128.0.1", "140.82.112.3"] {
            assert!(!blocked(ip), "{ip} is public");
        }
    }

    #[test]
    fn blocks_v6_forms_that_embed_private_v4() {
        for ip in [
            "::1", "::", "::ffff:127.0.0.1", "::ffff:169.254.169.254", "::ffff:10.0.0.1", "::127.0.0.1", "64:ff9b::a9fe:a9fe",
            "2002:7f00:0001::1", "fd00:ec2::254", "fc00::1", "fe80::1", "ff02::1", "2001:db8::1", "fec0::1", "2001:0:4136:e378::1",
        ] {
            assert!(blocked(ip), "{ip} must be blocked");
        }
        for ip in ["2606:4700:4700::1111", "2a00:1450:4001:80b::200e", "::ffff:8.8.8.8", "2002:0808:0808::1"] {
            assert!(!blocked(ip), "{ip} is public");
        }
    }

    #[test]
    fn host_allowlist_exact_and_wildcard() {
        let allow = vec!["api.open-meteo.com".to_string(), "*.wikipedia.org".to_string()];
        assert!(host_allowed("api.open-meteo.com", &allow));
        assert!(host_allowed("API.Open-Meteo.com.", &allow));
        assert!(host_allowed("en.wikipedia.org", &allow));
        assert!(!host_allowed("wikipedia.org", &allow), "wildcard means subdomains only");
        assert!(!host_allowed("evilwikipedia.org", &allow));
        assert!(!host_allowed("api.open-meteo.com.evil.io", &allow));
        assert!(!host_allowed("open-meteo.com", &allow));
    }
}
