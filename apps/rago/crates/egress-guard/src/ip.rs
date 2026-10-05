//! Which addresses an outbound request must never reach. A port of
//! `apps/product-analytics/src/exports/net-guard.ts` (`isPrivateAddress`):
//! private, loopback, link-local, cloud metadata, carrier-grade NAT, and every
//! other reserved range, judged on the IPv4 address carried inside IPv6 forms
//! (IPv4-mapped, IPv4-compatible, 6to4) too. For IPv6 only global unicast
//! (`2000::/3`) outside its reserved blocks is reachable.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// `(network, prefix bits)`.
const V4_BLOCKED: [([u8; 4], u32); 15] = [
    ([0, 0, 0, 0], 8),       // "this" network
    ([10, 0, 0, 0], 8),      // private
    ([100, 64, 0, 0], 10),   // carrier-grade NAT (includes 100.100.100.200 metadata)
    ([127, 0, 0, 0], 8),     // loopback
    ([169, 254, 0, 0], 16),  // link-local (includes 169.254.169.254 metadata)
    ([172, 16, 0, 0], 12),   // private
    ([192, 0, 0, 0], 24),    // IETF protocol assignments
    ([192, 0, 2, 0], 24),    // documentation
    ([192, 88, 99, 0], 24),  // 6to4 relay anycast
    ([192, 168, 0, 0], 16),  // private
    ([198, 18, 0, 0], 15),   // benchmarking
    ([198, 51, 100, 0], 24), // documentation
    ([203, 0, 113, 0], 24),  // documentation
    ([224, 0, 0, 0], 4),     // multicast
    ([240, 0, 0, 0], 4),     // reserved + broadcast
];

/// Whether an IPv4 address must never be contacted.
pub fn v4_blocked(ip: Ipv4Addr) -> bool {
    let ip = u32::from(ip);
    V4_BLOCKED.iter().any(|(net, bits)| {
        let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
        ip & mask == u32::from_be_bytes(*net) & mask
    })
}

fn embedded_v4(g: &[u16; 8], at: usize) -> Ipv4Addr {
    let [a, b] = g[at].to_be_bytes();
    let [c, d] = g[at + 1].to_be_bytes();
    Ipv4Addr::new(a, b, c, d)
}

/// Whether an IPv6 address must never be contacted.
pub fn v6_blocked(ip: Ipv6Addr) -> bool {
    let g = ip.segments();
    let zero_up_to = |n: usize| g[..n].iter().all(|x| *x == 0);
    // ::ffff:a.b.c.d (IPv4-mapped): judge the IPv4 address.
    if zero_up_to(5) && g[5] == 0xffff {
        return v4_blocked(embedded_v4(&g, 6));
    }
    // ::, ::1 and the deprecated IPv4-compatible block ::a.b.c.d.
    if zero_up_to(6) {
        return true;
    }
    // ::ffff:0:a.b.c.d (IPv4-translated, RFC 2765).
    if zero_up_to(4) && g[4] == 0xffff && g[5] == 0 {
        return true;
    }
    // 6to4 (2002::/16) carries an IPv4 address: refuse it when that is internal.
    if g[0] == 0x2002 {
        return v4_blocked(embedded_v4(&g, 1));
    }
    // Only global unicast 2000::/3 is reachable; this excludes ::/8,
    // 64:ff9b::/96 (NAT64), 100::/64 (discard), fc00::/7 (unique local,
    // includes AWS fd00:ec2::254), fe80::/10, fec0::/10 and ff00::/8.
    if g[0] & 0xe000 != 0x2000 {
        return true;
    }
    // 2001::/23 IETF assignments (Teredo, ORCHID, ...), 2001:db8::/32 and
    // 3fff::/20 documentation.
    (g[0] == 0x2001 && (g[1] < 0x0200 || g[1] == 0x0db8)) || (g[0] == 0x3fff && g[1] < 0x1000)
}

/// Whether an address is private, loopback, link-local, metadata or
/// otherwise reserved, and so must never be contacted.
pub fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => v6_blocked(v6),
    }
}

#[cfg(test)]
mod tests {
    use super::is_blocked;

    fn blocked(s: &str) -> bool {
        is_blocked(s.parse().unwrap())
    }

    #[test]
    fn internal_v4_ranges_are_blocked_and_public_ones_are_not() {
        for ip in [
            "0.0.0.0", "10.1.2.3", "100.64.0.1", "100.100.100.200", "127.0.0.1", "169.254.169.254", "172.16.0.1",
            "172.31.255.255", "192.0.0.8", "192.0.2.1", "192.88.99.1", "192.168.1.1", "198.18.0.1", "198.19.255.255",
            "198.51.100.1", "203.0.113.9", "224.0.0.1", "240.0.0.1", "255.255.255.255",
        ] {
            assert!(blocked(ip), "{ip}");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "100.63.255.255", "100.128.0.1", "172.15.0.1", "172.32.0.1", "198.17.0.1", "93.184.216.34"] {
            assert!(!blocked(ip), "{ip}");
        }
    }

    #[test]
    fn v6_forms_carrying_an_internal_v4_or_reserved_are_blocked() {
        for ip in [
            "::", "::1", "::ffff:127.0.0.1", "::ffff:7f00:1", "::ffff:169.254.169.254", "::ffff:10.0.0.1", "::127.0.0.1",
            "::ffff:0:127.0.0.1", "64:ff9b::7f00:1", "100::1", "fc00::1", "fd00:ec2::254", "fe80::1", "fec0::1", "ff02::1",
            "2002:7f00:1::", "2002:a9fe:a9fe::1", "2001::1", "2001:db8::1", "3fff::1",
        ] {
            assert!(blocked(ip), "{ip}");
        }
        for ip in ["2606:4700:4700::1111", "2001:4860:4860::8888", "::ffff:8.8.8.8", "2002:808:808::1"] {
            assert!(!blocked(ip), "{ip}");
        }
    }
}
