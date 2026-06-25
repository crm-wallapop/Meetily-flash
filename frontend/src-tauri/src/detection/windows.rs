//! Windows adapter implementing `MeetingDetectorPort` via:
//!   - `EnumWindows` + `GetWindowTextW` + `GetWindowThreadProcessId` for window enumeration
//!   - `GetExtendedUdpTable` / `GetExtendedTcpTable` (iphlpapi) for network socket scanning
//!
//! All Win32 calls are confined to this file. The rest of the codebase sees only the port trait.

#![cfg(target_os = "windows")]

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::ports::meeting_detector::{DetectorObservation, MeetWindow, MeetingDetectorPort};

use super::google_cidrs::{is_in_google_cidrs, is_in_turn_cidrs};

// ── Constants ─────────────────────────────────────────────────────────────

// Chrome and Edge release the getUserMedia WASAPI capture session within ~1-2s of "Leave call".
// Firefox and Brave are included in detection but their capture-session release timing on
// Meet leave is unverified; exit detection may be delayed or blocked for those browsers.
const BROWSER_PROCESSES: &[&str] = &["chrome.exe", "msedge.exe", "firefox.exe", "brave.exe"];

/// Maximum number of (title, instant) entries kept in the focus history.
const FOCUS_HISTORY_CAP: usize = 10;

/// How long to retain focus history entries.
const FOCUS_HISTORY_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// Minimum continuous `has_browser_capture_session()` run preceding the call's FIRST
/// `true → false` drop required to classify the exit as stable (4 s UDP debounce).
/// Below this the exit is transient-prone (15 s). Chosen above the spec's ~10 s WASAPI
/// transient ceiling with margin: a real meeting holds capture for minutes, so the guard
/// is satisfied for essentially all genuine calls; only pathologically short or flaky
/// sessions fall back to 15 s (the safe direction). A `const` (not configurable — YAGNI).
pub const STABLE_CONFIDENCE_WINDOW: Duration = Duration::from_secs(20);

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
        let history = focus_history.lock().unwrap_or_else(|e| e.into_inner());
        let cutoff = Instant::now().checked_sub(FOCUS_HISTORY_TTL).unwrap_or_else(Instant::now);
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
    RE.get_or_init(|| Regex::new(r"^Meet - .+|^Meet \u{2013} .+|^Google Meet - Meet \u{2014} .+|.+ - Google Meet$").expect("meet title regex is valid"))
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
                // thread_local — no cross-thread poisoning possible; unwrap is safe
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

    ENUM_RESULTS.with(|r| r.lock().unwrap().clear()); // thread_local — unwrap safe

    unsafe {
        let _ = EnumWindows(Some(enum_windows_callback), LPARAM(0));
    }

    ENUM_RESULTS.with(|r| r.lock().unwrap().clone()) // thread_local — unwrap safe
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
            let raw_ip = IpAddr::V6(Ipv6Addr::from(*remote));
            // Unwrap IPv4-mapped addresses (::ffff:x.x.x.x) so dual-stack hosts
            // are matched against the IPv4 CIDR table where the ranges live.
            let ip = match raw_ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(raw_ip),
                other => other,
            };
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

// ── WASAPI browser capture detection ──────────────────────────────────────

// Raw binding that returns the CoInitializeEx HRESULT as a plain i32, preserving
// the S_OK (0) vs S_FALSE (1) distinction. The windows-rs safe wrapper maps both
// to Ok(()), but we need to know which: S_OK means we initialised COM on this
// thread and must call CoUninitialize; S_FALSE means it was already initialised
// and we must NOT call CoUninitialize (D3).
#[link(name = "ole32")]
extern "system" {
    #[link_name = "CoInitializeEx"]
    fn co_initialize_ex_raw(pv_reserved: *const core::ffi::c_void, dw_co_init: u32) -> i32;
}
const COINIT_APARTMENTTHREADED_RAW: u32 = 0x2;
// Thread already initialised with a different concurrency model (MTA). COM is
// still fully usable on that thread; IMMDeviceEnumerator works from both STA and
// MTA. This happens when the tokio thread pool reuses a thread that cpal/WASAPI
// audio capture previously initialised as MTA (after recording starts).
const RPC_E_CHANGED_MODE: i32 = 0x80010106_u32 as i32;

fn check_browser_capture_session_inner() -> windows::core::Result<bool> {
    use windows::core::Interface; // brings .cast::<T>() into scope for COM QI
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl2,
        IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
        eCapture,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let device_count = collection.GetCount()?;
        for i in 0..device_count {
            let device = collection.Item(i)?;
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let sessions = manager.GetSessionEnumerator()?;
            let session_count = sessions.GetCount()?;
            for j in 0..session_count {
                let session = sessions.GetSession(j)?;
                let state = session.GetState()?;
                // Log all non-expired sessions for diagnostics; only Active browser sessions
                // satisfy the exit signal (D10: Inactive = background/permission-only session).
                if state != AudioSessionStateExpired {
                    let session2: IAudioSessionControl2 = session.cast()?;
                    let pid = session2.GetProcessId()?;
                    let proc_name = process_name_for_pid(pid).unwrap_or_default();
                    let display_name = session2.GetDisplayName()
                        .ok()
                        .and_then(|s| s.to_string().ok())
                        .unwrap_or_default();
                    let session_id = session2.GetSessionIdentifier()
                        .ok()
                        .and_then(|s| s.to_string().ok())
                        .unwrap_or_default();
                    log::debug!(
                        "capture session: device={i} session={j} pid={pid} proc={proc_name:?} state={state:?} name={display_name:?} id={session_id:?}"
                    );
                    if state == AudioSessionStateActive && is_browser_process(pid) {
                        log::debug!("has_browser_capture_session: hit (Active) — pid={pid} proc={proc_name:?} name={display_name:?}");
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

/// RAII guard that calls `CoUninitialize` on drop. Ensures the COM ref count is
/// balanced even if a panic unwinds past the call site.
struct CoUninitGuard;
impl Drop for CoUninitGuard {
    fn drop(&mut self) {
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

/// Returns `true` if any known browser process holds an `AudioSessionStateActive` WASAPI
/// capture session.
///
/// Chrome/Edge hold the `getUserMedia` session in `Active` state throughout a Meet call
/// (even when muted — Chrome uses `track.enabled=false`, not `track.stop()`, so
/// `IAudioClient::Start()` keeps the endpoint streaming). On "Leave call" the session
/// transitions to `Inactive` within ~1–2 s. Background sessions for tabs that have mic
/// permission but are not currently streaming remain `Inactive` indefinitely.
///
/// This is the exit signal used by `step_detector`'s InCall branch (D2 asymmetric):
/// exit debounce starts when this returns `false`, independent of TCP state. This avoids
/// false meeting-ended events caused by 90s+ TCP drops observed during active UDP calls.
pub fn has_browser_capture_session() -> bool {
    unsafe {
        // The detection loop is tokio::spawn (async). After each sleep().await the future
        // may resume on a different thread, so COM must be initialised per-call (D3).
        let hr = co_initialize_ex_raw(core::ptr::null(), COINIT_APARTMENTTHREADED_RAW);
        // RPC_E_CHANGED_MODE: thread already in MTA (e.g. tokio reused a cpal audio
        // thread). COM is usable; IMMDeviceEnumerator works from MTA. Proceed without
        // re-initializing and without uninitializing.
        // All other negative HRESULTs: COM genuinely unavailable — degrade to false.
        if hr < 0 && hr != RPC_E_CHANGED_MODE {
            return false;
        }
        // S_OK (0) and S_FALSE (1) both increment the COM ref count per MSDN —
        // both require CoUninitialize. RPC_E_CHANGED_MODE is negative (excluded above)
        // and must NOT be balanced. The guard handles panic and early returns uniformly.
        let _guard = if hr >= 0 { Some(CoUninitGuard) } else { None };
        check_browser_capture_session_inner().unwrap_or(false)
    }
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
            let raw_ip = IpAddr::V6(Ipv6Addr::from(*remote));
            // Unwrap IPv4-mapped addresses (::ffff:x.x.x.x) so dual-stack hosts
            // are matched against the IPv4 CIDR table where the ranges live.
            let ip = match raw_ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(raw_ip),
                other => other,
            };
            if is_in_google_cidrs(ip) && is_browser_process(row.dwOwningPid) {
                return true;
            }
        }
        false
    }
}

// ── WindowsMeetingDetector ─────────────────────────────────────────────────

/// Injectable probes replacing the four Win32 free functions in test builds.
/// The `meet_windows` probe is required alongside the network/WASAPI probes because
/// `current_state()` branches on window presence before reaching the network checks —
/// without it, adapter-layer tests that need non-empty windows would always see the
/// empty-windows code path and never reach the TURN/WASAPI logic.
///
/// Gated `#[cfg(test)]` so release builds compile this struct out entirely.
#[cfg(test)]
pub(crate) struct DetectorProbes {
    pub has_turn: Box<dyn Fn() -> bool + Send + Sync>,
    pub has_conn: Box<dyn Fn() -> bool + Send + Sync>,
    pub has_capture: Box<dyn Fn() -> bool + Send + Sync>,
    pub meet_windows: Box<dyn Fn() -> Vec<MeetWindow> + Send + Sync>,
}

pub struct WindowsMeetingDetector {
    #[cfg(not(test))]
    detector_start: Instant,
    #[cfg(test)]
    pub(crate) detector_start: Instant,
    first_poll_done: bool,
    connection_first_seen_at: Option<Instant>,
    pub focus_history: FocusHistory,
    /// True once a TURN relay connection has been observed for the current call.
    /// Reset by `notify_exit()` (called by the use case after `MeetingEnded`) so
    /// back-to-back calls are detectable. `pub(crate)` so adapter tests can set it
    /// directly without going through a real TURN connection.
    pub(crate) turn_established: bool,
    /// Previous value of `has_browser_capture_session`. `None` before the first poll.
    /// Used to emit an `info`-level log on every `bc` transition so smoke tests can
    /// measure the exact WASAPI-drop lag after "Leave call" without needing `RUST_LOG=debug`.
    last_bc: Option<bool>,
    /// Start of the current unbroken `bc == true` run. Set on a `false → true` edge
    /// and on the first poll if `bc == true` (stamped to `self.now()`, the first-poll
    /// instant — the unknowable pre-start history cannot be recovered, so a mid-call
    /// app start measures conservatively). Cleared to `None` on a `true → false` edge
    /// AFTER the run length is read. Once `exit_stable_latch` is `Some`, this field is
    /// no longer consulted (the decision is locked). `pub(crate)` so adapter tests can
    /// read it.
    pub(crate) bc_true_since: Option<Instant>,
    /// Locked-first-drop exit-stability decision (design D1–D3). `None` until the
    /// call's FIRST `bc` `true → false` drop; on that drop set ONCE to
    /// `Some(run_len >= STABLE_CONFIDENCE_WINDOW)` where `run_len = now − bc_true_since`.
    /// Once `Some(v)`, IMMUTABLE until `notify_exit()` — neither a `false → true`
    /// recovery nor a later drop may change it. This immutability is the anti-self-heal
    /// guarantee: `step_detector` recomputes the debounce every poll, so the
    /// `stable_capture` value driving it must not flip mid-debounce (the recovery-based
    /// draft recreated the detector-turn-latch trap of commit 693ff90). `pub(crate)` so
    /// adapter tests can read it.
    pub(crate) exit_stable_latch: Option<bool>,
    /// Injectable probes for unit tests — absent in release builds (D test seam).
    #[cfg(test)]
    pub(crate) probes: Option<DetectorProbes>,
    /// Test-overridable clock — absent in release builds. Production reads
    /// `Instant::now()`; tests that drive ≥ `STABLE_CONFIDENCE_WINDOW` runs
    /// deterministically inject a controlled instant. The `now()` helper is the ONLY
    /// sanctioned way to read the current instant inside `current_state()` so every
    /// read is seam-divertible (design task 1.14).
    #[cfg(test)]
    pub(crate) clock: Option<Box<dyn Fn() -> Instant + Send + Sync>>,
}

impl WindowsMeetingDetector {
    pub fn new(focus_history: FocusHistory) -> Self {
        Self {
            detector_start: Instant::now(),
            first_poll_done: false,
            connection_first_seen_at: None,
            focus_history,
            turn_established: false,
            last_bc: None,
            bc_true_since: None,
            exit_stable_latch: None,
            #[cfg(test)]
            probes: None,
            #[cfg(test)]
            clock: None,
        }
    }

    /// The current instant. Production: `Instant::now()`. Tests: a controlled clock may
    /// be injected via `clock` so ≥ `STABLE_CONFIDENCE_WINDOW` runs are driven
    /// deterministically without real sleeps. EVERY `Instant::now()` read inside
    /// `current_state()` MUST go through this helper (design task 1.14) — a partial seam
    /// lets tests pass while production diverges.
    fn now(&self) -> Instant {
        #[cfg(test)]
        {
            if let Some(clock) = &self.clock {
                return clock();
            }
        }
        Instant::now()
    }
}

#[cfg(test)]
impl WindowsMeetingDetector {
    /// Constructs a detector with injectable probes so adapter-layer tests can
    /// control Win32 outputs without real browser windows or network state.
    pub(crate) fn with_probes(focus_history: FocusHistory, probes: DetectorProbes) -> Self {
        let mut det = Self::new(focus_history);
        det.probes = Some(probes);
        det
    }
}

impl MeetingDetectorPort for WindowsMeetingDetector {
    fn notify_exit(&mut self) {
        // Reset the sticky TURN flag so the next call (potentially UDP-only)
        // goes through the full join/exit detection flow again (D7).
        self.turn_established = false;
        // Reset the locked-first-drop latch + run-length timer so the next call decides
        // stability afresh. A transient-prone previous call must not poison the next;
        // an immutable latch must not survive across calls (design D4).
        self.exit_stable_latch = None;
        self.bc_true_since = None;
    }

    fn current_state(&mut self) -> DetectorObservation {
        #[cfg(test)]
        let meet_windows = self.probes.as_ref()
            .map_or_else(enumerate_meet_windows, |p| (p.meet_windows)());
        #[cfg(not(test))]
        let meet_windows = enumerate_meet_windows();

        log::debug!(
            "detector poll: windows={:?}",
            meet_windows.iter().map(|w| format!("{}(pid={})", w.title, w.pid)).collect::<Vec<_>>(),
        );

        // Always check the connection signals regardless of window state.
        // meet_windows.is_empty() must not short-circuit exit detection: a transient
        // tab switch makes windows disappear for several seconds while the user is
        // still in the call and both network signals stay true. Gating exit on windows
        // caused false `meeting-ended` events whenever focus moved away from the Meet tab.
        // step_detector already guards entry via `has_title && has_conn`, so empty windows
        // only prevent join — never exit.
        #[cfg(test)]
        let turn = self.probes.as_ref()
            .map_or_else(has_turn_connection, |p| (p.has_turn)());
        #[cfg(not(test))]
        let turn = has_turn_connection();

        // Compute WASAPI capture state unconditionally for two uses:
        //   • entry conjunction (UDP phase below): mc && bc
        //   • exit signal in observation.has_browser_capture_session (asymmetric D2):
        //     step_detector InCall branch uses this, not has_conn, so 90s+ TCP drops
        //     during an active call do not start the exit debounce.
        #[cfg(test)]
        let bc = self.probes.as_ref()
            .map_or_else(has_browser_capture_session, |p| (p.has_capture)());
        #[cfg(not(test))]
        let bc = has_browser_capture_session();

        if self.last_bc != Some(bc) {
            log::info!(
                "bc transition: {} → {} (WASAPI browser capture {})",
                self.last_bc.map_or("?", |v| if v { "true" } else { "false" }),
                bc,
                if bc { "active" } else { "dropped" },
            );
            // Locked-first-drop latch (design D1–D3). Decide exit stability ONCE, at the
            // call's first true→false drop, from the unbroken true-run preceding it.
            // Once exit_stable_latch is Some(v) it is IMMUTABLE until notify_exit() —
            // neither a recovery nor a later drop may change it. This is the
            // anti-self-heal guarantee: step_detector recomputes the debounce every
            // poll, so stable_capture must not flip mid-debounce (the recovery-based
            // draft recreated the detector-turn-latch trap of commit 693ff90).
            let dropped = self.last_bc == Some(true) && !bc;
            let recovered = self.last_bc == Some(false) && bc;
            if self.exit_stable_latch.is_none() {
                if dropped {
                    let run_len = self
                        .bc_true_since
                        .map(|start| self.now().saturating_duration_since(start))
                        .unwrap_or(Duration::ZERO);
                    self.exit_stable_latch = Some(run_len >= STABLE_CONFIDENCE_WINDOW);
                    self.bc_true_since = None;
                } else if recovered {
                    self.bc_true_since = Some(self.now());
                }
            }
            self.last_bc = Some(bc);
        }

        // Latch TURN-established only when a TURN relay coincides with an active
        // browser capture session. `bc` is the in-call discriminator the detector
        // already relies on for UDP entry/exit — non-Meet GCP traffic (Gmail, Drive)
        // has no capture session, so this stops it from poisoning is_turn_exit.
        if turn && bc {
            self.turn_established = true;
        }

        // Entry signal: `turn || (mc && bc)` unconditionally. The prior
        // `else if turn_established { false }` arm is removed — its rationale
        // ("prevent the exit debounce from starting") was stale: exit has used
        // `bc` (not has_conn) since the asymmetric-D2 redesign. The arm only
        // ever blocked entry, and forcing entry false while latched was a
        // self-reinforcing deadlock (notify_exit only fires on InCall→Idle,
        // which requires entry, which the arm prevented). See Decision 1.
        let has_conn = if turn {
            log::debug!("detector poll: has_turn_connection=true");
            true
        } else {
            // UDP/join phase: TURN relay not yet seen. AND both signals for entry so that:
            // • join is detected when mc && bc both true after getUserMedia opens, and
            // • the conjunction is discriminating enough to avoid false-positive entry.
            // Exit detection uses bc alone via has_browser_capture_session in the observation.
            #[cfg(test)]
            let mc = self.probes.as_ref()
                .map_or_else(has_meet_connection, |p| (p.has_conn)());
            #[cfg(not(test))]
            let mc = has_meet_connection();
            log::debug!("detector poll: has_meet_connection={mc} has_browser_capture_session={bc}");
            mc && bc
        };

        // connection_first_seen_at tracking (D15 pre-existing check):
        //   First poll: stamp any connection as pre-existing (no window gate — a minimized
        //   Meet window at startup would bypass D15 on the second poll if gated here).
        //   Subsequent polls: only set when a Meet window is also visible, so background
        //   Google TCP (Gmail, Drive) never poisons the check before the user opens Meet.
        //   Reset: only when has_conn drops to false.
        if !self.first_poll_done {
            self.first_poll_done = true;
            if has_conn {
                // D15: stamp any connection at startup as pre-existing regardless of
                // whether a Meet window is visible. Window gate removed from this branch:
                // a minimized Meet window on poll 1 would leave C_FSA = None, then poll 2
                // would set C_FSA = now() (> detector_start), making the connection look
                // new and bypassing D15.
                self.connection_first_seen_at = Some(self.detector_start);
            }
            // First-poll bc stamp (design First-poll semantics): if capture is already
            // active at the first poll (app started mid-call), the pre-start run length
            // is unknowable — stamp to the first-poll instant so a later exit's run_len
            // is measured conservatively from app start, not true capture onset. Uses
            // self.now() (not detector_start) so the test clock seam is coherent: a
            // controlled clock advances both this stamp and the drop-time read together.
            // If the first poll is bc=false (app started before getUserMedia opened),
            // bc_true_since stays None until the first false→true edge begins the run.
            if bc {
                self.bc_true_since = Some(self.now());
            }
        } else if has_conn && !meet_windows.is_empty() && self.connection_first_seen_at.is_none() {
            self.connection_first_seen_at = Some(self.now());
        } else if !has_conn {
            self.connection_first_seen_at = None;
        }

        // True when TURN was established for this call and has just dropped.
        // Tells the state machine to use the short TURN debounce (4 s) instead of
        // the 15 s UDP debounce, restoring the ~5 s exit latency for TCP TURN calls.
        let is_turn_exit = !turn && self.turn_established;

        let mut obs = DetectorObservation {
            meet_windows,
            has_meet_connection: has_conn,
            has_browser_capture_session: bc,
            connection_first_seen_at: self.connection_first_seen_at,
            default_title: String::new(),
            is_turn_exit,
            stable_capture: self.exit_stable_latch.unwrap_or(false),
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

                    let mut h = history.lock().unwrap_or_else(|e| e.into_inner());
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
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicBool, Ordering};

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
            has_browser_capture_session: true,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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

    // ── Task 3.1 — has_browser_capture_session ─────────────────────────────

    #[test]
    fn has_browser_capture_session_smoke() {
        // Returns a bool without panicking. CI has no browser with active mic, so
        // this should be false — but any value is valid if a browser capture is live.
        let _ = has_browser_capture_session();
    }

    #[test]
    #[ignore = "only valid in headless CI; fails when a browser holds an active capture session"]
    fn has_browser_capture_session_false_in_non_interactive_env() {
        // Adversarial: no browser process holds an active WASAPI capture session in
        // headless CI. Must return false without hanging or panicking.
        // Fails if run while a browser is actively capturing audio — correct behaviour.
        assert!(!has_browser_capture_session());
    }

    #[test]
    fn check_browser_capture_session_inner_no_sessions_returns_false() {
        // Adversarial (empty session list): when COM initialises but no browser holds
        // a capture session, the inner function must return Ok(false). In CI, there are
        // no browser audio sessions, so this exercises the zero-session code path.
        let result = check_browser_capture_session_inner();
        match result {
            Ok(v) => assert!(!v, "no browser capture session should be present in CI"),
            Err(_) => {} // COM unavailable in this test environment — also acceptable
        }
    }

    // ── Task 5.1 b/c — WindowsMeetingDetector adapter layer ───────────────

    fn probe_windows(titles: &'static [&'static str]) -> Box<dyn Fn() -> Vec<MeetWindow> + Send + Sync> {
        Box::new(move || {
            titles.iter().map(|t| MeetWindow { hwnd_id: 1, pid: 100, title: t.to_string() }).collect()
        })
    }

    // ── detector-turn-latch-deadlock — adversarial RED tests ───────────────
    //
    // These three tests assert the post-fix invariants and FAIL on the pre-fix
    // code (the entry-suppression arm + the turn-only latch set).

    // Task 1.1 — Deadlock regression: a latched turn_established flag must NOT
    // block detection of a subsequent real UDP call (mc && bc both true). The
    // pre-fix else-if turn_established arm forces has_conn=false here.
    #[test]
    fn latched_turn_flag_does_not_block_subsequent_udp_call_entry() {
        let probes = DetectorProbes {
            has_turn:    Box::new(|| false), // TURN gone (UDP call)
            has_conn:    Box::new(|| true),  // Meet TCP active
            has_capture: Box::new(|| true),  // browser mic capture active
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);
        // Simulate the poisoned state: a prior browser→GCP connection latched
        // the flag in a previous poll. This is the deadlock scenario.
        det.turn_established = true;

        let obs = det.current_state();
        assert!(obs.has_meet_connection,
            "a latched turn_established flag must NOT suppress entry when a real UDP call (mc && bc) is active");
    }

    // Task 1.2 — Spurious-latch non-poison: TURN active but NO browser capture
    // (background GCP traffic, no call) must NOT latch turn_established, so a
    // later UDP call is not misclassified as a TURN exit (4s debounce).
    #[test]
    fn turn_without_browser_capture_does_not_latch() {
        // Phase 1: TURN=true but bc=false (background GCP, no call).
        let probes_gcp = DetectorProbes {
            has_turn:    Box::new(|| true),
            has_conn:    Box::new(|| false),
            has_capture: Box::new(|| false),
            meet_windows: probe_windows(&[]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes_gcp);
        // Poll several times to give the latch every chance to (wrongly) set.
        for _ in 0..3 {
            let _ = det.current_state();
        }
        assert!(!det.turn_established,
            "turn_established must NOT set on TURN-without-capture (background GCP traffic)");

        // Phase 2: user joins a real UDP call (TURN=false, mc && bc true).
        det.probes = Some(DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: probe_windows(&["Meet - standup"]),
        });
        let obs = det.current_state();
        assert!(obs.has_meet_connection, "the UDP call must be detected");
        assert!(!obs.is_turn_exit,
            "is_turn_exit must be false — the prior GCP traffic must not poison the exit debounce");
    }

    // Task 1.3 — Entry formula invariant: with turn_established latched, the
    // entry signal has_meet_connection must equal `turn || (mc && bc)` across
    // the full probe matrix. The latch must never change the entry formula.
    #[test]
    fn entry_formula_invariant_holds_across_probe_matrix_when_latched() {
        for &turn in &[false, true] {
            for &mc in &[false, true] {
                for &bc in &[false, true] {
                    let probes = DetectorProbes {
                        has_turn:    Box::new(move || turn),
                        has_conn:    Box::new(move || mc),
                        has_capture: Box::new(move || bc),
                        meet_windows: probe_windows(&["Meet - standup"]),
                    };
                    let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);
                    // Latch is set (simulating a prior TURN call). The entry
                    // formula must still be `turn || (mc && bc)`.
                    det.turn_established = true;
                    let obs = det.current_state();
                    let expected = turn || (mc && bc);
                    assert_eq!(
                        obs.has_meet_connection, expected,
                        "turn={turn} mc={mc} bc={bc}: has_meet_connection must equal turn || (mc && bc), got {}",
                        obs.has_meet_connection
                    );
                }
            }
        }
    }

    // (b) notify_exit resets turn_established so the next UDP call is detectable.
    //     Post-detector-turn-latch-deadlock: the suppression arm is gone, so
    //     has_conn is true both before and after notify_exit when mc && bc are
    //     both true. The notify_exit → detectable-again assertion (the real
    //     purpose of this test) stays; only the stale "before = false" assertion
    //     (which encoded the deadlock) is corrected to true.
    #[test]
    fn notify_exit_resets_turn_established_for_next_udp_detection() {
        let probes = DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);
        det.turn_established = true;

        // Before notify_exit: suppression arm gone, mc && bc both true → has_conn=true
        let obs = det.current_state();
        assert!(obs.has_meet_connection,
            "post-fix: turn_established=true no longer suppresses entry when mc && bc are true");

        det.notify_exit();

        // After notify_exit: turn_established=false, UDP probes both true → has_conn=true
        let obs = det.current_state();
        assert!(obs.has_meet_connection,
            "after notify_exit(), UDP call should be detectable again");
    }

    // (b2) is_turn_exit computation: after TURN goes away on a call where it was
    // established, current_state() must return is_turn_exit=true so the state
    // machine can use the fast 4s debounce. This is the adapter-level twin of
    // the state machine tests 2.10 / 2.10b — it tests the computation, not the
    // state machine's response to it.
    #[test]
    fn turn_exit_flag_set_when_turn_drops_after_being_established() {
        let probes_turn_active = DetectorProbes {
            has_turn:    Box::new(|| true),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes_turn_active);

        // Poll 1: TURN active — turn_established becomes true, is_turn_exit must be false.
        let obs1 = det.current_state();
        assert!(!obs1.is_turn_exit, "is_turn_exit must be false while TURN is active");
        assert!(det.turn_established, "turn_established must be set after first TURN-active poll");

        // Poll 2: TURN drops — is_turn_exit must now be true.
        det.probes = Some(DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: probe_windows(&["Meet - standup"]),
        });
        let obs2 = det.current_state();
        assert!(obs2.is_turn_exit,
            "is_turn_exit must be true when TURN drops after being established (turn_established=true && !turn)");

        // Poll 3 (after notify_exit): turn_established is reset — is_turn_exit must be false again.
        det.notify_exit();
        let obs3 = det.current_state();
        assert!(!obs3.is_turn_exit,
            "is_turn_exit must be false after notify_exit() resets turn_established");
    }

    // Regression guard for the self-heal trap (commit 715c810, reverted 693ff90).
    // The real Chrome exit sequence is: TURN relay drops first (~1s), then WASAPI
    // browser-capture releases ~2s later. A self-heal that clears turn_established
    // on (!turn && !bc) fires on THIS poll — flipping is_turn_exit false mid-exit
    // and bumping the debounce 4s->15s, because step_detector recomputes the
    // duration every poll. turn_established must stay latched until notify_exit.
    #[test]
    fn turn_latch_survives_bc_drop_during_exit() {
        let probes_active = DetectorProbes {
            has_turn:    Box::new(|| true),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes_active);

        // Poll 1: TURN + capture active — latch sets.
        let obs1 = det.current_state();
        assert!(!obs1.is_turn_exit);
        assert!(det.turn_established);

        // Poll 2: TURN drops, WASAPI capture still active (~2s lag) — is_turn_exit true.
        det.probes = Some(DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: probe_windows(&["Meet - standup"]),
        });
        let obs2 = det.current_state();
        assert!(obs2.is_turn_exit, "TURN drop must set is_turn_exit while bc lags");

        // Poll 3: WASAPI releases capture (turn=false, bc=false) — the self-heal
        // trap poll. is_turn_exit MUST stay true; turn_established must NOT clear.
        det.probes = Some(DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| false),
            meet_windows: probe_windows(&["Meet - standup"]),
        });
        let obs3 = det.current_state();
        assert!(det.turn_established,
            "turn_established must survive the bc-drop poll (self-heal trap)");
        assert!(obs3.is_turn_exit,
            "is_turn_exit must stay true across the WASAPI bc-drop mid-exit");
    }

    // (d) D15 / C-NEW-2: Meet window minimized at startup — first poll sees empty
    //     window list while a Google TCP connection already exists. The first-poll
    //     branch must stamp C_FSA = detector_start unconditionally (no window gate).
    //     Without the fix, C_FSA would stay None on poll 1, then be set to
    //     Instant::now() on poll 2 when the window reappears — which is > detector_start,
    //     making not_preexisting=true and falsely firing meeting-detected.
    #[test]
    fn d15_minimized_window_at_startup_does_not_bypass_preexisting_check() {
        let probes_poll1 = DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: Box::new(|| vec![]),  // minimized at startup — empty
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes_poll1);

        // Poll 1: connection active, window not visible (minimized)
        let obs1 = det.current_state();
        assert!(
            obs1.connection_first_seen_at.is_some(),
            "first poll must stamp C_FSA even when Meet window is not visible",
        );
        let c_fsa_poll1 = obs1.connection_first_seen_at.unwrap();

        // Poll 2: window now visible (user un-minimized / switched back to Meet tab)
        det.probes = Some(DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: probe_windows(&["Meet - standup"]),
        });
        let obs2 = det.current_state();

        // C_FSA must not have been updated to Instant::now() — it was already stamped.
        // If it were updated, it would be > detector_start → not_preexisting=true →
        // step_detector would fire meeting-detected for a pre-existing call.
        assert_eq!(
            obs2.connection_first_seen_at,
            Some(c_fsa_poll1),
            "C_FSA must remain at detector_start after poll 2, not be updated to Instant::now()",
        );

        // Verify step_detector sees the connection as pre-existing and stays Idle.
        use crate::use_cases::meeting_detection::{
            step_detector, DetectorSettings, DetectorState,
        };
        use std::sync::atomic::AtomicBool;
        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs2,
            det.detector_start,
            std::time::Instant::now(),
            &AtomicBool::new(false),
            &DetectorSettings::default(),
        );
        assert!(
            matches!(state, DetectorState::Idle),
            "minimized-at-startup pre-existing connection must not transition to InCall",
        );
        assert!(
            events.is_empty(),
            "D15: must not emit meeting-detected for a connection present before detector started",
        );
    }

    // (c) Otter.ai scenario: persistent browser mic keeps WASAPI active across calls.
    //     Post-detector-turn-latch-deadlock: a latched turn_established flag no longer
    //     blocks entry. With mc && bc both true, has_meet_connection is true (entry not
    //     suppressed); after notify_exit() it is still true. The pre-fix version of this
    //     test asserted the deadlock ("must block UDP detection"); that assertion encoded
    //     the bug as desired behaviour. The Otter.ai lobby false-positive (HTTPS + WASAPI
    //     from another app) is the pre-existing known limitation at canonical-spec line 107
    //     — it is not latch-suppressed (notify_exit resets the latch at Idle, the same
    //     state the lobby scenario starts from).
    #[test]
    fn otter_ai_persistent_mic_blocked_until_notify_exit() {
        let probes = DetectorProbes {
            has_turn:    Box::new(|| false),  // TURN gone after TCP exit
            has_conn:    Box::new(|| true),   // lobby HTTPS still active
            has_capture: Box::new(|| true),  // Otter.ai keeps WASAPI open
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);
        det.turn_established = true;

        // Post-fix invariant: a latched flag does NOT block entry when a real UDP
        // call (mc && bc) is active. (mc=true, bc=true → has_conn=true.)
        for i in 0..3 {
            let obs = det.current_state();
            assert!(obs.has_meet_connection,
                "poll {i}: a latched turn_established must not suppress entry for a real UDP call (mc && bc)");
        }

        det.notify_exit();

        // After notify_exit: WASAPI still active → next UDP call is detectable
        let obs = det.current_state();
        assert!(obs.has_meet_connection,
            "after notify_exit(), persistent Otter.ai WASAPI does not block next call");
    }

    // ── meeting-udp-confidence-debounce — adversarial RED tests ─────────────
    //
    // These drive the REAL WindowsMeetingDetector latch (locked-first-drop, design
    // D1–D4) through bc drop/recovery/flicker sequences via a controllable clock +
    // bc flag. They FAIL on the pre-change code, which latched
    // `bc_drop_observed_this_call` ON the exit drop — making every exit report
    // stable_capture=false and the 4 s debounce unreachable.

    /// Test harness: a detector whose `bc` signal is an `AtomicBool` and whose clock
    /// is an `Arc<Mutex<Instant>>` advanced by `advance()`. Lets ≥ STABLE_CONFIDENCE_WINDOW
    /// runs be driven deterministically without real sleeps. `turn` sets the TURN probe
    /// (true for device-disconnect scenarios where TURN stays alive mid-call).
    fn latched_detector(
        turn: bool,
        bc_initial: bool,
    ) -> (
        WindowsMeetingDetector,
        Arc<AtomicBool>,
        Arc<Mutex<Instant>>,
    ) {
        let bc_flag = Arc::new(AtomicBool::new(bc_initial));
        let start = Instant::now();
        let clock = Arc::new(Mutex::new(start));
        let bc_for_probe = Arc::clone(&bc_flag);
        let clock_for_probe = Arc::clone(&clock);
        let probes = DetectorProbes {
            has_turn: Box::new(move || turn),
            has_conn: Box::new(|| true),
            has_capture: Box::new(move || bc_for_probe.load(Ordering::SeqCst)),
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);
        det.clock = Some(Box::new(move || *clock_for_probe.lock().unwrap()));
        (det, bc_flag, clock)
    }

    fn set_bc(flag: &Arc<AtomicBool>, v: bool) {
        flag.store(v, Ordering::SeqCst);
    }

    fn advance(clock: &Arc<Mutex<Instant>>, dur: Duration) {
        *clock.lock().unwrap() += dur;
    }

    // Task 1.1 — A bc=true run ≥ STABLE_CONFIDENCE_WINDOW followed by a single
    // bc=false drop latches Some(true) so the 4 s debounce applies. FAILS on the
    // pre-change code: the drop self-latches false (bc_drop_observed_this_call).
    #[test]
    fn first_drop_after_window_latches_stable_true() {
        let (mut det, bc, clock) = latched_detector(false, true);
        let _ = det.current_state();
        advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_secs(1));
        set_bc(&bc, false);
        let obs = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(true), "run ≥ window → latch Some(true)");
        assert!(obs.stable_capture, "stable_capture must be true so the 4 s debounce applies");
    }

    // Task 1.2 (self-heal guard, shark C1) — once Some(true) is set on the first
    // drop, a 1-poll WASAPI flicker (false → true → false) during the debounce
    // MUST NOT clear or flip the latch. The recovery-based draft failed this: it
    // cleared the latch on the false→true edge, flipping a running 4 s exit to 15 s.
    #[test]
    fn flicker_during_debounce_does_not_flip_latch() {
        let (mut det, bc, clock) = latched_detector(false, true);
        let _ = det.current_state();
        advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_secs(5));
        set_bc(&bc, false);
        let obs_drop = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(true));
        assert!(obs_drop.stable_capture);

        // 1-poll WASAPI flicker: bc returns true for one poll.
        set_bc(&bc, true);
        let obs_flicker = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(true), "flicker must NOT clear the latch");
        assert!(obs_flicker.stable_capture, "stable_capture stays true through the flicker");

        // Flicker ends: bc drops again. Latch still Some(true).
        set_bc(&bc, false);
        let obs_post = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(true), "latch immutable across the second drop");
        assert!(obs_post.stable_capture);
    }

    // Task 1.3 — A bc=true run shorter than the window, then a drop, latches
    // Some(false) → stable_capture false → 15 s debounce.
    #[test]
    fn short_stable_run_then_drop_is_15s() {
        let (mut det, bc, clock) = latched_detector(false, true);
        let _ = det.current_state();
        advance(&clock, Duration::from_secs(5)); // < 20 s window
        set_bc(&bc, false);
        let obs = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(false), "run < window → latch Some(false)");
        assert!(!obs.stable_capture, "stable_capture false → 15 s debounce");
    }

    // Task 1.4 (the D1 relaxation) — a call that ran stable ≥ window, suffered a
    // recovered transient, then really exits, STILL exits at 4 s: the first drop's
    // run was ≥ window so the latch is Some(true), and the recovery + later drop
    // cannot change it (immutability). The old "transient ⟹ 15 s" rule is relaxed.
    #[test]
    fn recovered_transient_after_window_still_exits_4s() {
        let (mut det, bc, clock) = latched_detector(false, true);
        let _ = det.current_state();
        advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_secs(5));
        set_bc(&bc, false); // first drop (the "transient"): latch Some(true)
        let obs1 = det.current_state();
        assert!(obs1.stable_capture);

        set_bc(&bc, true); // recovery + second stable run
        let _ = det.current_state();
        advance(&clock, STABLE_CONFIDENCE_WINDOW);
        set_bc(&bc, false); // real exit (second drop): latch held from the first drop
        let obs2 = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(true), "latch held from first drop");
        assert!(obs2.stable_capture, "the real exit still gets the 4 s debounce");
    }

    // Task 1.5 — After a stable exit latches Some(true), several consecutive
    // bc=false polls must all report stable_capture == true (step_detector
    // recomputes the debounce every poll, so the value must be stable across the
    // debounce window).
    #[test]
    fn latch_held_stable_across_consecutive_false_polls() {
        let (mut det, bc, clock) = latched_detector(false, true);
        let _ = det.current_state();
        advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_secs(1));
        set_bc(&bc, false);
        for i in 0..5 {
            let obs = det.current_state();
            assert_eq!(det.exit_stable_latch, Some(true));
            assert!(obs.stable_capture,
                "consecutive false poll {i}: stable_capture must stay true");
        }
    }

    // Task 1.6 — notify_exit() resets the latch so the next call's first drop is
    // classified afresh (no inheritance of the prior call's stability assessment).
    #[test]
    fn notify_exit_clears_latch_for_next_call() {
        let (mut det, bc, clock) = latched_detector(false, true);
        let _ = det.current_state();
        advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_secs(1));
        set_bc(&bc, false);
        let obs = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(true));
        assert!(obs.stable_capture);

        det.notify_exit();
        assert!(det.exit_stable_latch.is_none(), "notify_exit clears the latch");
        assert!(det.bc_true_since.is_none(), "notify_exit clears bc_true_since");

        // Next call: short run → Some(false). No inheritance of Some(true).
        set_bc(&bc, true);
        let _ = det.current_state();
        advance(&clock, Duration::from_secs(3));
        set_bc(&bc, false);
        let obs2 = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(false), "next call classified afresh");
        assert!(!obs2.stable_capture, "no inheritance of the prior stable latch");
    }

    // Task 1.7 (shark C2) — Rapid leave/rejoin within the ~2 s WASAPI release lag:
    // bc reads true throughout (capture was never released), so there is no
    // true→false edge and no latch must be set. bc_true_since stays continuous.
    #[test]
    fn rapid_leave_rejoin_within_wasapi_lag_no_spurious_drop() {
        let (mut det, _bc, clock) = latched_detector(false, true);
        let _ = det.current_state();
        let since_after_first = det.bc_true_since;
        assert!(since_after_first.is_some(), "first-poll bc=true stamps bc_true_since");

        for _ in 0..3 {
            advance(&clock, Duration::from_millis(500));
            let obs = det.current_state();
            assert!(det.exit_stable_latch.is_none(), "no drop → latch never set");
            assert!(!obs.stable_capture, "no drop → stable_capture stays false (default)");
        }
        assert_eq!(det.bc_true_since, since_after_first, "bc_true_since continuous, never reset");
    }

    // Task 1.8 (shark C1, rewritten) — A WASAPI device loss mid-call drives bc
    // false even though the user has not left (the enumerated device vanished).
    // This IS a true→false drop, so the latch fires and classifies by run length —
    // NOT by whether it was a "real leave". TURN relay stays alive so is_turn_exit
    // == false throughout. Two variants: ≥window disconnect → Some(true);
    // <window → Some(false).
    #[test]
    fn device_disconnect_mid_call_classified_by_run_length() {
        // Variant A: long run before the disconnect → Some(true).
        {
            let (mut det, bc, clock) = latched_detector(true, true);
            let _ = det.current_state();
            advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_secs(5));
            set_bc(&bc, false); // device lost
            let obs = det.current_state();
            assert!(!obs.is_turn_exit, "TURN alive → device loss is not a TURN exit");
            assert_eq!(det.exit_stable_latch, Some(true), "≥window disconnect → stable");
            assert!(obs.stable_capture);
        }
        // Variant B: short run before the disconnect → Some(false).
        {
            let (mut det, bc, clock) = latched_detector(true, true);
            let _ = det.current_state();
            advance(&clock, Duration::from_secs(3)); // < window
            set_bc(&bc, false);
            let obs = det.current_state();
            assert!(!obs.is_turn_exit);
            assert_eq!(det.exit_stable_latch, Some(false), "<window disconnect → unstable");
            assert!(!obs.stable_capture);
        }
    }

    // Task 1.9 (shark I2) — Detector constructed mid-call (bc already true at first
    // poll). The pre-start run length is unknowable, so bc_true_since is stamped to
    // the first-poll instant; an immediate drop measures run_len ≈ 0 < window →
    // Some(false) (conservative).
    #[test]
    fn mid_call_app_start_measures_short_run() {
        let (mut det, bc, clock) = latched_detector(false, true);
        let _ = det.current_state();
        assert!(det.bc_true_since.is_some());
        advance(&clock, Duration::from_millis(1));
        set_bc(&bc, false);
        let obs = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(false), "mid-call start + immediate drop → conservative 15 s");
        assert!(!obs.stable_capture);
    }

    // Task 1.10 (shark I4) — Pin the >= comparison at the boundary: run_len exactly
    // STABLE_CONFIDENCE_WINDOW → Some(true); window − ε → Some(false);
    // window + ε → Some(true).
    #[test]
    fn window_boundary_pins_gte() {
        // Exactly at window → true.
        {
            let (mut det, bc, clock) = latched_detector(false, true);
            let _ = det.current_state();
            advance(&clock, STABLE_CONFIDENCE_WINDOW);
            set_bc(&bc, false);
            let _ = det.current_state();
            assert_eq!(det.exit_stable_latch, Some(true), "run_len == window → true (>= comparison)");
        }
        // Window − ε → false.
        {
            let (mut det, bc, clock) = latched_detector(false, true);
            let _ = det.current_state();
            advance(&clock, STABLE_CONFIDENCE_WINDOW - Duration::from_millis(1));
            set_bc(&bc, false);
            let _ = det.current_state();
            assert_eq!(det.exit_stable_latch, Some(false), "window − ε → false");
        }
        // Window + ε → true.
        {
            let (mut det, bc, clock) = latched_detector(false, true);
            let _ = det.current_state();
            advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_millis(1));
            set_bc(&bc, false);
            let _ = det.current_state();
            assert_eq!(det.exit_stable_latch, Some(true), "window + ε → true");
        }
    }

    // Task 1.11 (shark C3) — If the app dies mid-debounce before notify_exit()
    // fires, the stale latch dies with the process. A freshly-constructed detector
    // starts with exit_stable_latch = None and bc_true_since = None (reconstruction
    // is the crash-path safety net; notify_exit covers the normal path).
    #[test]
    fn fresh_detector_after_crash_has_no_inherited_latch() {
        let (mut prior, bc, clock) = latched_detector(false, true);
        let _ = prior.current_state();
        advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_secs(1));
        set_bc(&bc, false);
        let _ = prior.current_state();
        assert_eq!(prior.exit_stable_latch, Some(true));
        // `prior` is dropped here (simulated crash) — no notify_exit.

        let fresh = WindowsMeetingDetector::new(empty_history());
        assert!(fresh.exit_stable_latch.is_none(), "fresh detector has no inherited latch");
        assert!(fresh.bc_true_since.is_none(), "fresh detector has no inherited bc_true_since");
    }

    // Task 1.16 (shark round-2 I-R1) — The detector's FIRST poll reads bc == false
    // (app started before the browser opened getUserMedia). bc_true_since stays None
    // through the initial false polls, then is stamped at the false→true edge. A
    // later exit's run_len is measured from that edge, NOT from detector start.
    // Every other §1 test starts bc == true on poll 1, so this is the sole cover of
    // the None→false first-poll path.
    #[test]
    fn first_poll_false_then_stable_run_measured_from_edge() {
        let (mut det, bc, clock) = latched_detector(false, false); // bc starts false
        let obs = det.current_state();
        assert!(det.bc_true_since.is_none(), "first-poll bc=false → bc_true_since stays None");
        assert!(!obs.stable_capture);

        for _ in 0..3 {
            advance(&clock, Duration::from_secs(2));
            let _ = det.current_state();
            assert!(det.bc_true_since.is_none(), "bc still false → bc_true_since still None");
        }

        // false→true edge: bc_true_since stamped at the edge instant, not detector start.
        let edge_time = *clock.lock().unwrap();
        set_bc(&bc, true);
        let _ = det.current_state();
        assert_eq!(det.bc_true_since, Some(edge_time), "stamped at the edge instant, not detector start");

        advance(&clock, STABLE_CONFIDENCE_WINDOW + Duration::from_secs(1));
        set_bc(&bc, false);
        let obs = det.current_state();
        assert_eq!(det.exit_stable_latch, Some(true), "run measured from the edge ≥ window → stable");
        assert!(obs.stable_capture);
    }

    // Task 1.15 (shark I5 + round-2 I-R2) — Property: for any bc poll sequence, once
    // exit_stable_latch == Some(v) it stays Some(v) until notify_exit(); and
    // Some(v) ⟹ ((run_len at the first drop) >= STABLE_CONFIDENCE_WINDOW) == v. The
    // generator biases true-run lengths to 1-15 polls (2-30 s at the 2 s poll
    // interval) so runs straddle the 20 s boundary; notify_exit() at random run
    // boundaries exercises multiple calls' first-drops. A naive bool generator almost
    // never yields a ≥window run, making the property vacuous.
    proptest! {
        #[test]
        fn locked_latch_invariant_holds(
            runs in prop::collection::vec((1u32..16u32, any::<bool>()), 1..20)
        ) {
            let (mut det, bc, clock) = latched_detector(false, false);
            let poll_interval = Duration::from_secs(2);
            let mut latched_value: Option<bool> = None;

            for (run_idx, &(num_polls, notify_exit_before)) in runs.iter().enumerate() {
                if notify_exit_before {
                    det.notify_exit();
                    latched_value = None;
                }
                // bc alternates each run (even=true, odd=false) so true→false drops occur.
                set_bc(&bc, run_idx % 2 == 0);

                for _ in 0..num_polls {
                    advance(&clock, poll_interval);
                    let bc_true_since_before = det.bc_true_since;
                    let now = *clock.lock().unwrap();
                    let obs = det.current_state();

                    // Invariant 1: once latched, the value is immutable until notify_exit.
                    if let Some(v) = latched_value {
                        prop_assert_eq!(det.exit_stable_latch, Some(v),
                            "latch immutable: stays Some({})", v);
                        prop_assert_eq!(obs.stable_capture, v);
                    }

                    // Invariant 2: the latched value equals (run_len >= window) at the first
                    // drop, where run_len is independently recomputed from the observed
                    // bc_true_since and the controlled clock.
                    if latched_value.is_none() && det.exit_stable_latch.is_some() {
                        let run_len = bc_true_since_before
                            .map(|start| now.saturating_duration_since(start))
                            .unwrap_or(Duration::ZERO);
                        let expected = run_len >= STABLE_CONFIDENCE_WINDOW;
                        prop_assert_eq!(
                            det.exit_stable_latch,
                            Some(expected),
                            "latch value must equal (run_len {:?} >= window) == {}",
                            run_len,
                            expected
                        );
                        latched_value = det.exit_stable_latch;
                    }
                }
            }
        }
    }

    // Task 4.1 — #[ignore] real-clock test: a stable exit (latch Some(true)) drives
    // meeting-ended within the 4 s debounce + poll slack on a REAL wall clock, NOT
    // the 15 s path. Runs via `cargo test -- --ignored`. The injected clock drives
    // the latch run-length (so no 20 s real sleep is needed); the debounce itself is
    // measured in real Instant::now() time — this is the latency guarantee under test.
    #[test]
    #[ignore = "wall-clock timing: sleeps ~5 s; run with cargo test -- --ignored"]
    fn stable_exit_fires_within_short_debounce_real_clock() {
        use crate::use_cases::meeting_detection::{
            step_detector, DetectorEvent, DetectorSettings, DetectorState,
        };

        let bc_flag = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(Mutex::new(Instant::now()));
        let bc_for_probe = Arc::clone(&bc_flag);
        let clock_for_probe = Arc::clone(&clock);
        let probes = DetectorProbes {
            has_turn: Box::new(|| false),
            has_conn: Box::new(|| true),
            has_capture: Box::new(move || bc_for_probe.load(Ordering::SeqCst)),
            meet_windows: probe_windows(&["Meet - real-clock"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);
        det.clock = Some(Box::new(move || *clock_for_probe.lock().unwrap()));

        let settings = DetectorSettings::default(); // SHORT=4 s, LONG=15 s
        let suppress = AtomicBool::new(false);
        let detector_start = Instant::now();
        let poll_interval = Duration::from_millis(500);
        let mut state = DetectorState::Idle;

        // First poll: bc=false → Idle, no connection.
        let (next, _) =
            step_detector(state, &det.current_state(), detector_start, Instant::now(), &suppress, &settings);
        state = next;

        // Join: advance the injected clock 1 s (C_FSA > detector_start → not
        // pre-existing) and set bc=true. bc_true_since stamps from the clock edge.
        *clock.lock().unwrap() += Duration::from_secs(1);
        bc_flag.store(true, Ordering::SeqCst);
        std::thread::sleep(poll_interval);
        let (next, events) =
            step_detector(state, &det.current_state(), detector_start, Instant::now(), &suppress, &settings);
        assert!(
            events.iter().any(|e| matches!(e, DetectorEvent::MeetingDetected { .. })),
            "join must fire meeting-detected"
        );
        state = next;

        // Advance the injected clock past the window, drop bc → latch Some(true).
        *clock.lock().unwrap() += STABLE_CONFIDENCE_WINDOW + Duration::from_secs(1);
        bc_flag.store(false, Ordering::SeqCst);
        let drop_time = Instant::now();

        // Poll until meeting-ended fires. Real-clock debounce must be ~4 s, not ~15 s.
        loop {
            let now = Instant::now();
            let (next, events) =
                step_detector(state, &det.current_state(), detector_start, now, &suppress, &settings);
            state = next;
            if events.iter().any(|e| matches!(e, DetectorEvent::MeetingEnded)) {
                let elapsed = now.duration_since(drop_time);
                assert!(elapsed >= Duration::from_secs(4),
                    "must not fire before the 4 s debounce; fired at {elapsed:?}");
                assert!(elapsed < Duration::from_secs(8),
                    "stable exit must fire within ~4-6 s (4 s debounce + poll slack), took {elapsed:?}");
                return;
            }
            if now.duration_since(drop_time) > Duration::from_secs(12) {
                panic!("stable exit did not fire within 12 s — likely on the 15 s path");
            }
            std::thread::sleep(poll_interval);
        }
    }
}
