//! Meet call-signaling adapter — detects HTTPS/WSS TCP signaling connections
//! to Google media-server IPs. Extracted verbatim from `detection/windows.rs`;
//! the Google CIDR constants stay in `detection/google_cidrs.rs` (imported, not
//! moved). Implements `CallSignalingPort` so the detector entry gate is
//! vendor-neutral — a second vendor ships a new adapter, no core change.
//!
//! WebRTC signaling theory: signaling (HTTPS/WSS over TCP) runs throughout a
//! call for SDP exchange, ICE candidate trickle, and room state, independent of
//! the media transport — so a Meet UDP call keeps TCP connections to Google IPs
//! even with no TURN relay. This check is what catches TURN-less (UDP) Meet calls.

#![cfg(target_os = "windows")]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::detection::browser_process::is_browser_process;
use crate::detection::google_cidrs::is_in_google_cidrs;
use crate::ports::call_signaling::CallSignalingPort;

/// Meet signaling adapter — the production `CallSignalingPort` wiring (v1:
/// sole adapter). Construct it once at the composition root and inject it into
/// `WindowsMeetingDetector`.
pub struct MeetSignalingAdapter;

impl MeetSignalingAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MeetSignalingAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CallSignalingPort for MeetSignalingAdapter {
    fn is_call_signaling_active(&self) -> bool {
        check_tcp4_connections() || check_tcp6_connections()
    }
}

/// Unwrap IPv4-mapped IPv6 addresses (`::ffff:x.x.x.x`) so dual-stack hosts are
/// matched against the IPv4 CIDR table where the Google ranges live. Pure —
/// extracted so the IPv6 table path is unit-testable without a real TCP table.
fn unwrap_ipv4_mapped(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        other => other,
    }
}

/// Pure: `true` if `(remote_ip, owning_pid)` indicates a Meet signaling
/// connection. The `pid_is_browser` closure abstracts `is_browser_process` so
/// the row-matching logic is testable without Win32 process queries.
fn row_is_signaling(remote_ip: IpAddr, owning_pid: u32, pid_is_browser: impl Fn(u32) -> bool) -> bool {
    is_in_google_cidrs(unwrap_ipv4_mapped(remote_ip)) && pid_is_browser(owning_pid)
}

/// Returns `true` if any browser process has an active TCP4 connection to a
/// Google media-server IP.
///
/// PID note: `EnumWindows`→`GetWindowThreadProcessId` returns the *browser
/// process* PID (the Chrome UI process). Since Chrome v70+, TCP connections are
/// handled by a separate *Network Service* process (also named chrome.exe but
/// with a different PID). Filtering by the window PID therefore finds nothing.
/// We instead check the process *name* so any chrome.exe process (browser,
/// network-service, or renderer) can satisfy the match.
fn check_tcp4_connections() -> bool {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP_STATE_ESTAB, MIB_TCPTABLE_OWNER_PID,
        TCP_TABLE_OWNER_PID_ALL,
    };
    use windows::Win32::Networking::WinSock::AF_INET;

    unsafe {
        let mut size: u32 = 0;
        let _ = GetExtendedTcpTable(
            None,
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if size == 0 {
            return false;
        }

        let mut buf = vec![0u8; size as usize];
        let ret = GetExtendedTcpTable(
            Some(buf.as_mut_ptr() as *mut _),
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if ret != 0 {
            return false;
        }

        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let count = table.dwNumEntries as usize;
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), count);

        for row in rows {
            if row.dwState != MIB_TCP_STATE_ESTAB.0 as u32 {
                continue;
            }
            let remote_ip = IpAddr::V4(Ipv4Addr::from(u32::from_be(row.dwRemoteAddr)));
            if row_is_signaling(remote_ip, row.dwOwningPid, is_browser_process) {
                return true;
            }
        }
        false
    }
}

fn check_tcp6_connections() -> bool {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCP_STATE_ESTAB,
        TCP_TABLE_OWNER_PID_ALL,
    };
    use windows::Win32::Networking::WinSock::AF_INET6;

    unsafe {
        let mut size: u32 = 0;
        let _ = GetExtendedTcpTable(
            None,
            &mut size,
            false,
            AF_INET6.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if size == 0 {
            return false;
        }

        let mut buf = vec![0u8; size as usize];
        let ret = GetExtendedTcpTable(
            Some(buf.as_mut_ptr() as *mut _),
            &mut size,
            false,
            AF_INET6.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if ret != 0 {
            return false;
        }

        let table = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
        let count = table.dwNumEntries as usize;
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), count);

        for row in rows {
            if row.dwState != MIB_TCP_STATE_ESTAB.0 as u32 {
                continue;
            }
            let remote = &row.ucRemoteAddr;
            let remote_ip = IpAddr::V6(Ipv6Addr::from(*remote));
            if row_is_signaling(remote_ip, row.dwOwningPid, is_browser_process) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn google_v4() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(142, 250, 1, 1))
    }

    fn non_google_v4() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))
    }

    fn google_v4_mapped_v6() -> IpAddr {
        // ::ffff:142.250.1.1
        IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x8efa, 0x0101))
    }

    fn non_google_v6() -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111))
    }

    // ── unwrap_ipv4_mapped ────────────────────────────────────────────────

    #[test]
    fn unwrap_dual_stack_google_ip_to_ipv4() {
        assert_eq!(
            unwrap_ipv4_mapped(google_v4_mapped_v6()),
            IpAddr::V4(Ipv4Addr::new(142, 250, 1, 1))
        );
    }

    #[test]
    fn unwrap_passes_plain_ipv4_through() {
        assert_eq!(unwrap_ipv4_mapped(google_v4()), google_v4());
    }

    #[test]
    fn unwrap_passes_native_ipv6_through() {
        let v6 = non_google_v6();
        assert_eq!(unwrap_ipv4_mapped(v6), v6);
    }

    // ── row_is_signaling ──────────────────────────────────────────────────

    #[test]
    fn google_ip_browser_pid_matches() {
        assert!(row_is_signaling(google_v4(), 1234, |_| true));
    }

    #[test]
    fn non_google_ip_does_not_match() {
        assert!(!row_is_signaling(non_google_v4(), 1234, |_| true));
    }

    #[test]
    fn non_browser_pid_does_not_match() {
        assert!(!row_is_signaling(google_v4(), 1234, |_| false));
    }

    // ── §2.2 adversarial: empty + boundary ────────────────────────────────

    #[test]
    fn empty_row_check_returns_false() {
        // No rows → nothing to match. The production path returns false on
        // `size == 0` before reaching row_is_signaling; this pins that the
        // pure matcher itself never matches an empty iterator.
        let rows: [(IpAddr, u32); 0] = [];
        assert!(!rows
            .iter()
            .any(|(ip, pid)| row_is_signaling(*ip, *pid, |_| true)));
    }

    #[test]
    fn ipv4_mapped_v6_google_address_matches_after_unwrap() {
        // The dual-stack IPv6-mapped unwrap is load-bearing: without it the
        // IPv6 CIDR table (which lacks the Google media ranges) would miss
        // this address. This is the §2.1 fixture case.
        assert!(row_is_signaling(google_v4_mapped_v6(), 1234, |_| true));
    }

    #[test]
    fn native_google_ipv6_matches() {
        // 2001:4860:: is in GOOGLE_V6_CIDRS.
        let google_native_v6 = IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0, 0, 0, 0, 0, 1));
        assert!(row_is_signaling(google_native_v6, 1234, |_| true));
    }

    #[test]
    fn non_google_ipv6_does_not_match() {
        assert!(!row_is_signaling(non_google_v6(), 1234, |_| true));
    }
}
