//! Windows adapter implementing `MeetingDetectorPort` via:
//!   - `EnumWindows` + `GetWindowTextW` + `GetWindowThreadProcessId` for window enumeration
//!   - `GetExtendedUdpTable` / `GetExtendedTcpTable` (iphlpapi) for network socket scanning
//!
//! All Win32 calls are confined to this file. The rest of the codebase sees only the port trait.

#![cfg(target_os = "windows")]

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use regex::Regex;

use crate::ports::meeting_detector::{DetectorObservation, MeetWindow, MeetingDetectorPort};

use super::google_cidrs::{is_in_google_cidrs, is_in_turn_cidrs};

// ── Constants ─────────────────────────────────────────────────────────────

const BROWSER_PROCESSES: &[&str] = &["chrome.exe", "msedge.exe", "firefox.exe"];

/// Maximum number of (title, instant) entries kept in the focus history.
const FOCUS_HISTORY_CAP: usize = 10;

/// How long to retain focus history entries.
const FOCUS_HISTORY_TTL: std::time::Duration = std::time::Duration::from_secs(600);

// ── Focus tracker ─────────────────────────────────────────────────────────

/// Shared history of recently-focused Meet windows (title, moment).
pub type FocusHistory = Arc<Mutex<VecDeque<(String, Instant)>>>;

// ── Title resolution ──────────────────────────────────────────────────────

/// Resolves the best default title for a `meeting-detected` event using the
/// priority chain from D10.
pub fn resolve_default_title(
    observation: &DetectorObservation,
    focus_history: &FocusHistory,
) -> String {
    let re = meet_title_regex();

    if let Some(fg_title) = foreground_window_title() {
        if re.is_match(&fg_title) {
            return strip_google_meet_suffix(&fg_title);
        }
    }

    {
        let history = focus_history.lock().unwrap();
        let cutoff = Instant::now() - FOCUS_HISTORY_TTL;
        if let Some((title, _)) = history.iter().rev().find(|(_, t)| *t >= cutoff) {
            return strip_google_meet_suffix(title);
        }
    }

    if let Some(win) = observation.meet_windows.first() {
        return strip_google_meet_suffix(&win.title);
    }

    let now = chrono::Local::now();
    format!("Meeting {}", now.format("%Y-%m-%d %H:%M"))
}

pub(crate) fn strip_google_meet_suffix(title: &str) -> String {
    if let Some(name) = title.strip_prefix("Meet - ") {
        name.trim().to_string()
    } else if title.starts_with("Google Meet - Meet ") {
        title.split('\u{2014}')  // em dash
            .nth(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| title.to_string())
    } else if let Some(rest) = title.strip_prefix("Meet \u{2013} ") {
        // Edge tab-group collapsed: "Meet – <code> and N more pages - <group> - Microsoft Edge"
        rest.split_once(" and ").map(|(code, _)| code).unwrap_or(rest).trim().to_string()
    } else if let Some(name) = title.strip_suffix(" - Google Meet") {
        name.trim().to_string()
    } else {
        title.to_string()
    }
}

// ── Meet title regex ──────────────────────────────────────────────────────

fn meet_title_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    // Matches title formats observed in the wild:
    //   Chrome/Edge tab: "Meet - <name>"
    //   PWA:             "Google Meet - Meet — <name>"
    RE.get_or_init(|| Regex::new(r"^Meet - .+|^Meet \u{2013} .+|^Google Meet - Meet \u{2014}|.+ - Google Meet$").expect("meet title regex is valid"))
}

// ── Win32 helpers ─────────────────────────────────────────────────────────

fn foreground_window_title() -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn process_name_for_pid(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::core::PWSTR;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let result = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size);
        // HANDLE has no Drop impl in windows-rs — must close explicitly.
        let _ = CloseHandle(handle);
        result.ok()?;
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_lowercase())
    }
}

// ── Window enumeration ─────────────────────────────────────────────────────

thread_local! {
    static ENUM_RESULTS: Mutex<Vec<MeetWindow>> = Mutex::new(Vec::new());
}

unsafe extern "system" fn enum_windows_callback(
    hwnd: windows::Win32::Foundation::HWND,
    _lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::BOOL {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };

    if IsWindowVisible(hwnd).0 == 0 {
        return BOOL(1);
    }

    let mut buf = [0u16; 512];
    let len = GetWindowTextW(hwnd, &mut buf);
    if len == 0 {
        return BOOL(1);
    }
    let title = String::from_utf16_lossy(&buf[..len as usize]);

    // avoids regex overhead for the common non-Meet case
    if !title.starts_with("Meet - ")
        && !title.starts_with("Meet \u{2013} ")   // en dash — Edge tab-group collapsed format
        && !title.starts_with("Google Meet - Meet ")
        && !title.ends_with(" - Google Meet")
    {
        return BOOL(1);
    }

    let re = meet_title_regex();
    if !re.is_match(&title) {
        return BOOL(1);
    }

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return BOOL(1);
    }

    if let Some(name) = process_name_for_pid(pid) {
        if BROWSER_PROCESSES.contains(&name.as_str()) {
            ENUM_RESULTS.with(|r| {
                r.lock().unwrap().push(MeetWindow {
                    hwnd_id: hwnd.0 as *const () as usize,
                    pid,
                    title,
                });
            });
        }
    }

    BOOL(1)
}

/// All top-level Meet windows for browser processes in the allowlist.
pub fn enumerate_meet_windows() -> Vec<MeetWindow> {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

    ENUM_RESULTS.with(|r| r.lock().unwrap().clear());

    unsafe {
        let _ = EnumWindows(Some(enum_windows_callback), LPARAM(0));
    }

    ENUM_RESULTS.with(|r| r.lock().unwrap().clone())
}

// ── Network socket scanning ────────────────────────────────────────────────

/// Returns `true` if any browser process has an active TCP connection to a
/// Google media-server IP.
///
/// TCP-only rationale: `GetExtendedUdpTable` has no remote-addr field (UDP is
/// connectionless), so filtering by remote IP is impossible for UDP sockets.
/// TCP connections (`GetExtendedTcpTable`) carry remote addr and are present
/// during the HTTPS/WebSocket signalling phase that begins when a user joins.
///
/// PID note: `EnumWindows`→`GetWindowThreadProcessId` returns the *browser
/// process* PID (the Chrome UI process). Since Chrome v70+, TCP connections
/// are handled by a separate *Network Service* process (also named chrome.exe
/// but with a different PID). Filtering by the window PID therefore finds
/// nothing. We instead check the process *name* so any chrome.exe process
/// (browser, network-service, or renderer) can satisfy the match.
pub fn has_meet_connection() -> bool {
    check_tcp4_connections() || check_tcp6_connections()
}

/// Returns `true` if any browser process has an active TCP connection to a
/// Google TURN relay server.
///
/// TURN connections exist only during a live WebRTC call. The Meet lobby page
/// connects to general Google IPs (HTTPS) but never to TURN relay ranges. This
/// makes TURN presence a reliable "still in call" signal that drops as soon as
/// the user hangs up, even if the window title stays the same (Edge collapsed
/// tab group) and even though the lobby page also has Google TCP connections.
pub fn has_turn_connection() -> bool {
    check_turn_tcp4_connections() || check_turn_tcp6_connections()
}

fn check_turn_tcp4_connections() -> bool {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP_STATE_ESTAB, MIB_TCPTABLE_OWNER_PID,
        TCP_TABLE_OWNER_PID_ALL,
    };
    use windows::Win32::Networking::WinSock::AF_INET;

    unsafe {
        let mut size: u32 = 0;
        let _ = GetExtendedTcpTable(None, &mut size, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0);
        if size == 0 { return false; }

        let mut buf = vec![0u8; size as usize];
        let ret = GetExtendedTcpTable(Some(buf.as_mut_ptr() as *mut _), &mut size, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0);
        if ret != 0 { return false; }

        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let count = table.dwNumEntries as usize;
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), count);

        for row in rows {
            if row.dwState != MIB_TCP_STATE_ESTAB.0 as u32 { continue; }
            let remote_ip = IpAddr::V4(Ipv4Addr::from(u32::from_be(row.dwRemoteAddr)));
            if is_in_turn_cidrs(remote_ip) && is_browser_process(row.dwOwningPid) {
                return true;
            }
        }
        false
    }
}

fn check_turn_tcp6_connections() -> bool {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCP_STATE_ESTAB,
        TCP_TABLE_OWNER_PID_ALL,
    };
    use windows::Win32::Networking::WinSock::AF_INET6;

    unsafe {
        let mut size: u32 = 0;
        let _ = GetExtendedTcpTable(None, &mut size, false, AF_INET6.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0);
        if size == 0 { return false; }

        let mut buf = vec![0u8; size as usize];
        let ret = GetExtendedTcpTable(Some(buf.as_mut_ptr() as *mut _), &mut size, false, AF_INET6.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0);
        if ret != 0 { return false; }

        let table = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
        let count = table.dwNumEntries as usize;
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), count);

        for row in rows {
            if row.dwState != MIB_TCP_STATE_ESTAB.0 as u32 { continue; }
            let remote = &row.ucRemoteAddr;
            let ip = IpAddr::V6(Ipv6Addr::from(*remote));
            if is_in_turn_cidrs(ip) && is_browser_process(row.dwOwningPid) {
                return true;
            }
        }
        false
    }
}

/// Returns true if `pid` belongs to a known browser executable.
fn is_browser_process(pid: u32) -> bool {
    process_name_for_pid(pid)
        .map(|name| BROWSER_PROCESSES.contains(&name.as_str()))
        .unwrap_or(false)
}

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
            if is_in_google_cidrs(remote_ip) && is_browser_process(row.dwOwningPid) {
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
            let ip = IpAddr::V6(Ipv6Addr::from(*remote));
            if is_in_google_cidrs(ip) && is_browser_process(row.dwOwningPid) {
                return true;
            }
        }
        false
    }
}

// ── WindowsMeetingDetector ─────────────────────────────────────────────────

pub struct WindowsMeetingDetector {
    detector_start: Instant,
    first_poll_done: bool,
    connection_first_seen_at: Option<Instant>,
    pub focus_history: FocusHistory,
    /// True once a TURN relay connection has been observed for the current call.
    /// Stays true until meet_windows goes empty (tab closed / title changes).
    /// While true, `has_conn` is derived from TURN presence only — the Meet lobby
    /// page also has TCP connections to broad Google IPs, so broad-check alone
    /// would never start the exit debounce when leaving via the End-call button.
    turn_established: bool,
}

impl WindowsMeetingDetector {
    pub fn new(focus_history: FocusHistory) -> Self {
        Self {
            detector_start: Instant::now(),
            first_poll_done: false,
            connection_first_seen_at: None,
            focus_history,
            turn_established: false,
        }
    }
}

impl MeetingDetectorPort for WindowsMeetingDetector {
    fn current_state(&mut self) -> DetectorObservation {
        let meet_windows = enumerate_meet_windows();

        log::debug!(
            "detector poll: windows={:?}",
            meet_windows.iter().map(|w| &w.title).collect::<Vec<_>>(),
        );

        let has_conn = if meet_windows.is_empty() {
            // No Meet window visible: reset TURN tracking so the next call goes
            // through the full join-detection flow again.
            self.turn_established = false;
            false
        } else {
            let turn = has_turn_connection();
            if turn {
                self.turn_established = true;
            }

            if turn {
                log::debug!("detector poll: has_turn_connection=true");
                true
            } else if self.turn_established {
                // TURN was active for this call but is now gone → user hung up.
                // The lobby page maintains HTTPS connections to general Google IPs,
                // so falling back to the broad check here would prevent the exit
                // debounce from ever starting (the real bug this fixes).
                log::debug!("detector poll: TURN gone after call — treating as disconnected");
                false
            } else {
                // TURN not yet established: we're in the join handshake phase
                // (HTTPS signaling precedes TURN by 1-3 s). Use the broad check
                // so we detect the call as soon as signaling begins.
                let conn = has_meet_connection();
                log::debug!("detector poll: has_meet_connection={} (join phase, TURN not yet seen)", conn);
                conn
            }
        };

        if !self.first_poll_done {
            self.first_poll_done = true;
            if has_conn {
                // D15: connection existed before detector started — must not trigger detection
                self.connection_first_seen_at = Some(self.detector_start);
            }
        } else if has_conn && self.connection_first_seen_at.is_none() {
            self.connection_first_seen_at = Some(Instant::now());
        } else if !has_conn {
            self.connection_first_seen_at = None;
        }

        let mut obs = DetectorObservation {
            meet_windows,
            has_meet_connection: has_conn,
            connection_first_seen_at: self.connection_first_seen_at,
            default_title: String::new(),
        };

        if !obs.meet_windows.is_empty() {
            obs.default_title = resolve_default_title(&obs, &self.focus_history);
        }

        obs
    }
}

// ── Focus tracker task ─────────────────────────────────────────────────────

pub fn spawn_focus_tracker(history: FocusHistory) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let re = meet_title_regex();
            if let Some(title) = foreground_window_title() {
                if re.is_match(&title) {
                    let now = Instant::now();
                    let cutoff = now
                        .checked_sub(FOCUS_HISTORY_TTL)
                        .unwrap_or_else(Instant::now);

                    let mut h = history.lock().unwrap();
                    h.push_back((title, now));
                    while h.front().map_or(false, |(_, t)| *t < cutoff) {
                        h.pop_front();
                    }
                    while h.len() > FOCUS_HISTORY_CAP {
                        h.pop_front();
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_obs(titles: &[&str]) -> DetectorObservation {
        DetectorObservation {
            meet_windows: titles
                .iter()
                .map(|t| MeetWindow {
                    hwnd_id: 1,
                    pid: 100,
                    title: t.to_string(),
                })
                .collect(),
            has_meet_connection: true,
            connection_first_seen_at: None,
            default_title: String::new(),
        }
    }

    fn empty_history() -> FocusHistory {
        Arc::new(Mutex::new(VecDeque::new()))
    }

    // Chrome/Edge tab format: "Meet - <name>"
    #[test]
    fn title_parsing_chrome_tab_format() {
        let re = meet_title_regex();
        assert!(re.is_match("Meet - test"));
        assert!(re.is_match("Meet - abc-defg-hij"));
        assert!(re.is_match("Meet - Weekly sync"));
        assert_eq!(strip_google_meet_suffix("Meet - test"), "test");
        assert_eq!(strip_google_meet_suffix("Meet - Weekly sync"), "Weekly sync");
        assert!(!re.is_match("Meet - ")); // nothing after prefix — no match
    }

    // PWA format: "Google Meet - Meet — <Name>"
    #[test]
    fn title_parsing_pwa_format() {
        let re = meet_title_regex();
        assert!(re.is_match("Google Meet - Meet \u{2014} Test"));
        assert!(re.is_match("Google Meet - Meet \u{2014} Weekly Sync"));
        assert_eq!(strip_google_meet_suffix("Google Meet - Meet \u{2014} Test"), "Test");
        assert_eq!(strip_google_meet_suffix("Google Meet - Meet \u{2014} Weekly Sync"), "Weekly Sync");
    }

    // Suffix format used by newer Chrome/Edge: "<Name> - Google Meet"
    #[test]
    fn title_parsing_suffix_format() {
        let re = meet_title_regex();
        assert!(re.is_match("Sprint planning - Google Meet"));
        assert!(re.is_match("Q4 review - Google Meet"));
        assert_eq!(strip_google_meet_suffix("Sprint planning - Google Meet"), "Sprint planning");
        assert_eq!(strip_google_meet_suffix("Q4 review - Google Meet"), "Q4 review");
        assert!(!re.is_match("Google Meet")); // lobby / no meeting name — must not trigger
    }

    // Edge tab-group collapsed format: "Meet – <code> and N more pages - <group> - Microsoft​ Edge"
    // The en dash (U+2013) and tab-group suffix are Edge-specific window title synthesis.
    #[test]
    fn title_parsing_edge_tabgroup_format() {
        let re = meet_title_regex();
        assert!(re.is_match("Meet \u{2013} add-acfj-djw and 19 more pages - Work - Microsoft\u{200b} Edge"));
        assert!(re.is_match("Meet \u{2013} abc-defg-hij and 3 more pages - Personal - Microsoft\u{200b} Edge"));
        assert_eq!(
            strip_google_meet_suffix("Meet \u{2013} add-acfj-djw and 19 more pages - Work - Microsoft\u{200b} Edge"),
            "add-acfj-djw"
        );
        assert_eq!(
            strip_google_meet_suffix("Meet \u{2013} abc-defg-hij and 3 more pages - Personal - Microsoft\u{200b} Edge"),
            "abc-defg-hij"
        );
        // Single-tab edge case: no " and N more pages" suffix
        assert_eq!(
            strip_google_meet_suffix("Meet \u{2013} abc-defg-hij"),
            "abc-defg-hij"
        );
    }

    #[test]
    fn title_parsing_non_meet_does_not_match() {
        let re = meet_title_regex();
        assert!(!re.is_match("Chat with team about Google Meet"));
        assert!(!re.is_match("Sprint planning - YouTube"));
        assert!(!re.is_match("Zoom Meeting"));
    }

    // Task 4.1
    #[test]
    fn adversarial_4_1_non_meet_titles_not_matched() {
        let re = meet_title_regex();
        assert!(!re.is_match("Chat with team about Google Meet"));
        assert!(!re.is_match("Google Meet tips - YouTube"));
        assert!(re.is_match("Meet - standup"));
        assert!(re.is_match("Google Meet - Meet \u{2014} Standup"));
    }

    // Task 4.2: injection titles pass through as opaque text.
    #[test]
    fn adversarial_4_2_injection_titles_pass_through() {
        let re = meet_title_regex();
        let sql = "Meet - '; DROP TABLE meetings; --";
        let path = "Meet - ../../etc/passwd";
        assert!(re.is_match(sql));
        assert!(re.is_match(path));
        assert_eq!(strip_google_meet_suffix(sql), "'; DROP TABLE meetings; --");
        assert_eq!(strip_google_meet_suffix(path), "../../etc/passwd");
    }

    // Task 4.3: unicode / emoji titles.
    #[test]
    fn adversarial_4_3_unicode_emoji_titles() {
        let re = meet_title_regex();
        assert!(re.is_match("Meet - 📊 Q4 review"));
        assert_eq!(strip_google_meet_suffix("Meet - 📊 Q4 review"), "📊 Q4 review");
        assert!(re.is_match("Meet - مراجعة Q4"));
        assert!(re.is_match("Google Meet - Meet \u{2014} 📊 Q4"));
        assert_eq!(strip_google_meet_suffix("Google Meet - Meet \u{2014} 📊 Q4"), "📊 Q4");
    }

    #[test]
    fn resolve_default_title_fallback_is_non_empty() {
        let obs = make_obs(&["Meet - Sprint planning"]);
        let history = empty_history();
        let title = resolve_default_title(&obs, &history);
        assert!(!title.is_empty());
    }
}
