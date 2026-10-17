//! Telling a website name from an IP address, and the public internet from
//! the owner's own PC and home network.
//!
//! An allowlisted name can be pointed anywhere by whoever runs its DNS, so
//! the gate checks where a name actually leads before connecting. Anything
//! that is not plainly on the public internet is refused: the owner's PC,
//! their router and other home devices, Docker's own networks, and Tailscale.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `host` (as it appeared in a request, lower-cased) is an IP
/// address rather than a website name. Besides the usual forms this catches
/// the odd spellings some resolvers still accept, such as `2130706433` or
/// `0x7f.1`, by refusing any name whose last part is a number.
pub fn is_ip_literal(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return true;
    }
    let last = host.rsplit('.').next().unwrap_or(host);
    !last.is_empty() && (last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x"))
}

/// Whether an address is on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let private = a == 0 // "this network", including 0.0.0.0
        || a == 10
        || a == 127
        || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT; Tailscale lives here
        || (a == 169 && b == 254) // link-local, including cloud metadata
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 0 && c == 0) // protocol assignments
        || (a == 192 && b == 0 && c == 2) // documentation
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19)) // benchmarking
        || (a == 198 && b == 51 && c == 100) // documentation
        || (a == 203 && b == 0 && c == 113) // documentation
        || a >= 224; // multicast, reserved 240/4 and broadcast
    !private
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    // Addresses that carry an IPv4 address inside are judged by it, so
    // `::ffff:192.168.1.1` cannot sneak past as "IPv6".
    if let Some(v4) = embedded_v4(ip) {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    // Everything public sits in 2000::/3. Outside it are unique-local
    // fc00::/7, link-local fe80::/10, multicast ff00::/8 and other
    // special ranges.
    if s[0] & 0xe000 != 0x2000 {
        return false;
    }
    let special = (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x2001 && s[1] == 0x0000) // Teredo tunnels
        || (s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0) // benchmarking
        || (s[0] == 0x3fff && s[1] < 0x1000); // documentation (3fff::/20)
    !special
}

/// The IPv4 address inside an IPv4-mapped (`::ffff:a.b.c.d`), old
/// IPv4-compatible (`::a.b.c.d`), NAT64 (`64:ff9b::a.b.c.d`) or 6to4
/// (`2002:aabb:ccdd::`) address.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let tail = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    match s {
        [0, 0, 0, 0, 0, 0xffff, hi, lo] | [0, 0, 0, 0, 0, 0, hi, lo] => Some(tail(hi, lo)),
        [0x64, 0xff9b, 0, 0, 0, 0, hi, lo] => Some(tail(hi, lo)),
        [0x2002, hi, lo, ..] => Some(tail(hi, lo)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(s: &str) -> bool {
        is_public(s.parse().expect("test address"))
    }

    #[test]
    fn ip_literals_are_spotted() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "[::1]",
            "[2606:4700::1111]",
            "::1",
            "2130706433",
            "127.1",
            "0x7f.0.0.1",
            "1.2.3.4.",
        ] {
            assert!(is_ip_literal(ip), "{ip}");
        }
        for name in [
            "example.com",
            "api.stripe.com",
            "1password.com",
            "abc.cafe",
            "x.y.",
        ] {
            assert!(!is_ip_literal(name), "{name}");
        }
    }

    #[test]
    fn home_and_special_v4_ranges_are_not_public() {
        for ip in [
            "0.0.0.0",
            "0.1.2.3",
            "10.1.2.3",
            "127.0.0.1",
            "100.64.0.1",
            "100.101.102.103",
            "100.127.255.255",
            "169.254.169.254",
            "172.16.0.1",
            "172.17.0.2",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!public(ip), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.0.1",
            "172.32.0.1",
            "192.0.1.1",
            "192.169.0.1",
            "198.20.0.1",
            "223.255.255.255",
        ] {
            assert!(public(ip), "{ip}");
        }
    }

    #[test]
    fn home_and_special_v6_ranges_are_not_public() {
        for ip in [
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "2001:2::1",
            "3fff::1",
            "100::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.10",
            "::ffff:100.100.1.1",
            "::10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::192.168.0.1",
            "2002:c0a8:0101::1",
            "2002:7f00:1::",
        ] {
            assert!(!public(ip), "{ip}");
        }
        for ip in [
            "2606:4700:4700::1111",
            "2a00:1450:4001::200e",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:0808:0808::1",
        ] {
            assert!(public(ip), "{ip}");
        }
    }
}
