//! Shared Win32 browser-process identification used by both the TCP signaling
//! scan (`CallSignalingPort` adapter) and the WASAPI capture-session check
//! (`WindowsMeetingDetector`). Lives outside `windows.rs` so the extracted
//! signaling adapter can import it without a circular dependence on the
//! monolithic adapter file (design D2).

#![cfg(target_os = "windows")]

/// Browser executables recognised as conference-call hosts. Chrome and Edge
/// release the `getUserMedia` WASAPI capture session within ~1–2 s of "Leave
/// call". Firefox and Brave are included in detection but their capture-session
/// release timing on Meet leave is unverified; exit detection may be delayed or
/// blocked for those browsers.
pub(crate) const BROWSER_PROCESSES: &[&str] =
    &["chrome.exe", "msedge.exe", "firefox.exe", "brave.exe"];

/// Returns `true` if `pid` belongs to a known browser executable.
pub(crate) fn is_browser_process(pid: u32) -> bool {
    process_name_for_pid(pid)
        .map(|name| BROWSER_PROCESSES.contains(&name.as_str()))
        .unwrap_or(false)
}

/// Returns the lowercased executable file name for `pid` (e.g. `chrome.exe`),
/// or `None` if the process cannot be opened or queried.
pub(crate) fn process_name_for_pid(pid: u32) -> Option<String> {
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
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    // §4 boundary: pid 0 is the idle pseudo-process under Windows and is never
    // a browser. Defends `OpenProcess` callers against the sentinel value.
    #[test]
    fn idle_pseudo_pid_is_not_a_browser() {
        assert!(!is_browser_process(0));
    }

    // A pid this high is effectively never assigned by NT; `OpenProcess` returns
    // an error and `is_browser_process` falls through to `false`. Pins the
    // none-or-error → false contract rather than panicking.
    #[test]
    fn nonexistent_pid_is_not_a_browser() {
        assert!(!is_browser_process(999_999));
    }
}
