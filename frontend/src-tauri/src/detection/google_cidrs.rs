//! Hardcoded Google media-server CIDR ranges used to detect Meet WebRTC connections.
//!
//! # Refresh policy
//! These ranges are derived from Google's published ASN data (AS15169) and the
//! dedicated media-server subnets documented at:
//!   https://support.google.com/a/answer/1247360
//!
//! Review and update annually (or after significant Google infrastructure changes).
//! Auto-refresh from ASN data is tracked as v2 work (D18).
//!
//! Last reviewed: 2026-05

use ipnet::{Ipv4Net, Ipv6Net};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::OnceLock;

/// Representative Google-owned IPv4 CIDR ranges (media servers + general Google infra).
/// Covers the subnets most commonly observed in Meet WebRTC captures.
static GOOGLE_V4_CIDRS: &[&str] = &[
    // Google primary ranges (AS15169)
    "142.250.0.0/15",
    "172.217.0.0/16",
    "173.194.0.0/16",
    "74.125.0.0/16",
    "64.233.160.0/19",
    "66.102.0.0/20",
    "66.249.64.0/19",
    "72.14.192.0/18",
    "209.85.128.0/17",
    "216.58.192.0/19",
    "216.239.32.0/19",
    // Google Meet / TURN server ranges
    "34.64.0.0/10",
    "35.190.0.0/17",
    "35.191.0.0/16",
    "130.211.0.0/22",
];

/// TURN/relay-server-only CIDRs — a strict subset of GOOGLE_V4_CIDRS.
/// These ranges host WebRTC relay servers and are only active during a live call;
/// the Meet lobby page never connects to them. Used for "still in call" detection.
static TURN_V4_CIDRS: &[&str] = &[
    "34.64.0.0/10",
    "35.190.0.0/17",
    "35.191.0.0/16",
    "130.211.0.0/22",
];

/// Representative Google-owned IPv6 CIDR ranges.
static GOOGLE_V6_CIDRS: &[&str] = &[
    "2001:4860::/32",
    "2404:6800::/32",
    "2607:f8b0::/32",
    "2800:3f0::/32",
    "2a00:1450::/32",
    "2c0f:fb50::/32",
];

// ── Parsed-network caches ─────────────────────────────────────────────────
// Parsing CIDRs on every TCP-row check was O(rows × cidrs) string operations.
// These OnceLocks pay the parse cost once at first use.

fn google_v4_nets() -> &'static Vec<Ipv4Net> {
    static V4: OnceLock<Vec<Ipv4Net>> = OnceLock::new();
    V4.get_or_init(|| {
        GOOGLE_V4_CIDRS.iter().filter_map(|s| Ipv4Net::from_str(s).ok()).collect()
    })
}

fn google_v6_nets() -> &'static Vec<Ipv6Net> {
    static V6: OnceLock<Vec<Ipv6Net>> = OnceLock::new();
    V6.get_or_init(|| {
        GOOGLE_V6_CIDRS.iter().filter_map(|s| Ipv6Net::from_str(s).ok()).collect()
    })
}

fn turn_v4_nets() -> &'static Vec<Ipv4Net> {
    static V4: OnceLock<Vec<Ipv4Net>> = OnceLock::new();
    V4.get_or_init(|| {
        TURN_V4_CIDRS.iter().filter_map(|s| Ipv4Net::from_str(s).ok()).collect()
    })
}

// ── Public API ────────────────────────────────────────────────────────────

/// Returns `true` if `ip` falls within any of the hardcoded Google media CIDR ranges.
pub fn is_in_google_cidrs(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => google_v4_nets().iter().any(|net| net.contains(&v4)),
        IpAddr::V6(v6) => google_v6_nets().iter().any(|net| net.contains(&v6)),
    }
}

/// Returns `true` if `ip` is a Google TURN/relay server.
/// TURN connections only exist during an active Meet call, not on the lobby page.
pub fn is_in_turn_cidrs(ip: IpAddr) -> bool {
    match ip {
        // TURN servers are IPv4; skip IPv6 to avoid false positives from general Google infra.
        IpAddr::V4(v4) => turn_v4_nets().iter().any(|net| net.contains(&v4)),
        IpAddr::V6(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn known_google_ipv4_matches() {
        // 8.8.8.8 is Google DNS (not media), but 142.250.x.x is Google media.
        let google_media = IpAddr::V4(Ipv4Addr::new(142, 250, 1, 1));
        assert!(is_in_google_cidrs(google_media));

        let google_74 = IpAddr::V4(Ipv4Addr::new(74, 125, 0, 1));
        assert!(is_in_google_cidrs(google_74));
    }

    #[test]
    fn non_google_ipv4_does_not_match() {
        let cloudflare = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
        assert!(!is_in_google_cidrs(cloudflare));

        let loopback = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert!(!is_in_google_cidrs(loopback));

        // Discord / Hetzner range
        let discord = IpAddr::V4(Ipv4Addr::new(162, 159, 128, 1));
        assert!(!is_in_google_cidrs(discord));
    }

    #[test]
    fn known_google_ipv6_matches() {
        let google_v6 = IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888));
        assert!(is_in_google_cidrs(google_v6));
    }

    #[test]
    fn non_google_ipv6_does_not_match() {
        let cloudflare_v6 = IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111));
        assert!(!is_in_google_cidrs(cloudflare_v6));
    }

    #[test]
    fn turn_cidrs_are_subset_of_google_cidrs() {
        // A TURN IP must also match the broad Google check.
        let turn_ip = IpAddr::V4(Ipv4Addr::new(34, 100, 0, 1)); // 34.64.0.0/10
        assert!(is_in_turn_cidrs(turn_ip));
        assert!(is_in_google_cidrs(turn_ip));
    }

    #[test]
    fn non_turn_google_ip_does_not_match_turn() {
        // 74.125.x.x is a general Google range (signaling/HTTPS), not TURN.
        let signaling_ip = IpAddr::V4(Ipv4Addr::new(74, 125, 0, 1));
        assert!(is_in_google_cidrs(signaling_ip));
        assert!(!is_in_turn_cidrs(signaling_ip));
    }

    #[test]
    fn turn_cidrs_ipv6_always_false() {
        let google_v6 = IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0, 0, 0, 0, 0, 1));
        assert!(!is_in_turn_cidrs(google_v6));
    }
}
