## Why

`meeting-ended` never fires for Google Meet calls that use UDP media transport (the default on typical networks). After leaving such a call, Chrome/Edge stays on the `meet.google.com/<code>` lobby page — title unchanged, HTTPS TCP connections to Google still open — so the existing TCP-only fallback detection never clears, the debounce never starts, and the auto-stop prompt never appears. Users are left with a recording that runs indefinitely.

## What Changes

- Add `has_browser_capture_session()` to `detection/windows.rs`: a WASAPI COM query that returns `true` when any browser process (`chrome.exe`, `msedge.exe`, `firefox.exe`, `brave.exe`) holds an active audio capture session. Chrome/Edge hold the `getUserMedia` stream open throughout a Meet call (even when muted) and release it within ~1–2 s of leaving — this is the signal the TCP path cannot see.
- Change the UDP-call exit logic from `has_meet_connection()` to `has_meet_connection() && has_browser_capture_session()`. The AND requires both a Google TCP connection (Meet page context) AND an active browser capture session (call active). On the lobby page after leaving, the Google TCP connection remains but the capture session is gone — the AND evaluates to `false`, starting the 4 s debounce.
- Add `Win32_Media_Audio` to the `windows` feature set in `Cargo.toml` (required for `IMMDeviceEnumerator`, `IAudioSessionManager2`, `IAudioSessionControl2` COM interfaces).
- Update the `meeting-auto-detect` delta spec to reflect the corrected exit detection behaviour and remove the now-fixed `resume_all()` known-bug note.

## Capabilities

### New Capabilities

_(none — this is a bug fix inside the existing meeting-detector capability)_

### Modified Capabilities

- `meeting-auto-detect`: The exit detection requirement changes for the UDP case. Previously "connection to Google media IPs becomes absent for 10 s"; corrected to "Google TCP connection AND browser WASAPI capture session both absent for 4 s (UDP path)" or "TURN connection absent for 4 s (TCP TURN path, unchanged)". Also: the `resume_all()` known-bug note is updated to `[Fixed 2026-05-18]`.

## Impact

- `frontend/src-tauri/src/detection/windows.rs` — new `has_browser_capture_session()` function (~60–80 LOC), updated `current_state()` branch
- `frontend/src-tauri/Cargo.toml` — `Win32_Media_Audio` added to windows features
- `openspec/specs/meeting-auto-detect/spec.md` — delta spec amends exit detection requirement and fixes stale bug note
- No API surface changes; no frontend changes; no backend changes
