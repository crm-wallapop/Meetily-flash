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

// Chrome and Edge release the getUserMedia WASAPI capture session within ~1-2s of "Leave call".
// Firefox and Brave are included in detection but their capture-session release timing on
// Meet leave is unverified; exit detection may be delayed or blocked for those browsers.
const BROWSER_PROCESSES: &[&str] = &["chrome.exe", "msedge.exe", "firefox.exe", "brave.exe"];

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
    /// Per-call latch: flips to `true` on the first observed `bc` `Some(true) → false`
    /// transition (a WASAPI capture-session drop). `bc` stability is the in-call
    /// discriminator the detector already relies on — a drop proves the setup is
    /// transient-prone (device switch, brief WASAPI hiccup), so once observed the call
    /// uses the long 15 s UDP debounce for any subsequent exit; a stable-mic call (the
    /// common case) keeps the 4 s debounce. Monotonic false→true; reset only by
    /// `notify_exit()` on the `InCall → Idle` transition so back-to-back calls start
    /// fresh. `pub(crate)` so adapter tests can set it directly.
    pub(crate) bc_drop_observed_this_call: bool,
    /// Injectable probes for unit tests — absent in release builds (D test seam).
    #[cfg(test)]
    pub(crate) probes: Option<DetectorProbes>,
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
            bc_drop_observed_this_call: false,
            #[cfg(test)]
            probes: None,
        }
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
        // Reset the per-call bc-drop latch so the next call starts assumed-stable
        // (4 s debounce). A transient-prone previous call must not poison the next.
        self.bc_drop_observed_this_call = false;
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
            // Latch the per-call bc-drop flag on the first Some(true) → false
            // transition. Must read `last_bc` BEFORE reassigning it below. Once
            // observed, the call is treated as transient-prone for its remainder
            // (15 s UDP debounce); reset only by `notify_exit()`.
            if self.last_bc == Some(true) && !bc {
                self.bc_drop_observed_this_call = true;
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
                // would set C_FSA = Instant::now() (> detector_start), making the
                // connection look new and bypassing D15.
                self.connection_first_seen_at = Some(self.detector_start);
            }
        } else if has_conn && !meet_windows.is_empty() && self.connection_first_seen_at.is_none() {
            self.connection_first_seen_at = Some(Instant::now());
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
            stable_capture: !self.bc_drop_observed_this_call,
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

    // ── meeting-udp-media-signal — adversarial RED tests ────────────────────
    //
    // These pin the per-call bc-drop latch that drives the adaptive UDP debounce.
    // They FAIL on the pre-change code: `stable_capture` does not exist, so the
    // field cannot be read and the latch is never tripped.

    // Task 1.1 — A call with continuously-active bc (no drop) must report
    // stable_capture=true so step_detector selects the 4 s short debounce.
    #[test]
    fn stable_call_sets_stable_capture_true() {
        let probes = DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(|| true),
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);
        // Poll several times with bc continuously true. No Some(true) → false
        // transition occurs, so the latch stays false and stable_capture stays true.
        for i in 0..3 {
            let obs = det.current_state();
            assert!(obs.stable_capture,
                "poll {i}: continuously-stable bc must yield stable_capture=true");
        }
    }

    // Task 1.2 — The first bc drop (Some(true) → false) latches
    // bc_drop_observed_this_call=true. stable_capture must then be false on
    // every subsequent poll, EVEN AFTER bc returns true (the latch is monotonic
    // for the remainder of the call).
    #[test]
    fn first_bc_drop_latches_stable_capture_false() {
        let bc_flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let bc_for_probe = Arc::clone(&bc_flag);
        let probes = DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(move || bc_for_probe.load(std::sync::atomic::Ordering::SeqCst)),
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);

        // Poll 1: bc=true (stable so far).
        let obs = det.current_state();
        assert!(obs.stable_capture, "before any drop: stable_capture must be true");

        // Poll 2: bc drops (a transient). Latch trips.
        bc_flag.store(false, std::sync::atomic::Ordering::SeqCst);
        let obs = det.current_state();
        assert!(!obs.stable_capture,
            "after first Some(true) → false transition: stable_capture must be false");

        // Poll 3: bc returns. The latch must remain tripped (monotonic per call).
        bc_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        let obs = det.current_state();
        assert!(!obs.stable_capture,
            "after bc returns: the latch must stay tripped for the rest of the call");

        // Poll 4: bc drops again. Still false.
        bc_flag.store(false, std::sync::atomic::Ordering::SeqCst);
        let obs = det.current_state();
        assert!(!obs.stable_capture, "a second drop must keep stable_capture false");
    }

    // Task 1.3 — notify_exit() resets the latch so the next call starts
    // assumed-stable. A transient-prone previous call must not poison the next.
    #[test]
    fn notify_exit_resets_bc_drop_latch_for_next_call() {
        let bc_flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let bc_for_probe = Arc::clone(&bc_flag);
        let probes = DetectorProbes {
            has_turn:    Box::new(|| false),
            has_conn:    Box::new(|| true),
            has_capture: Box::new(move || bc_for_probe.load(std::sync::atomic::Ordering::SeqCst)),
            meet_windows: probe_windows(&["Meet - standup"]),
        };
        let mut det = WindowsMeetingDetector::with_probes(empty_history(), probes);

        // Establish bc=true first so the subsequent drop is a real Some(true) → false
        // transition (last_bc must be Some(true) before the drop poll).
        let _ = det.current_state();

        // Trip the latch with a drop during the first call.
        bc_flag.store(false, std::sync::atomic::Ordering::SeqCst);
        let obs = det.current_state();
        assert!(!obs.stable_capture, "first call: drop must trip the latch");

        // notify_exit fires on InCall → Idle. Adapter resets the latch.
        det.notify_exit();

        // Second call: bc continuously true. stable_capture must be true again.
        bc_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        for i in 0..3 {
            let obs = det.current_state();
            assert!(obs.stable_capture,
                "poll {i} after notify_exit: next call must start assumed-stable");
        }
    }
}
