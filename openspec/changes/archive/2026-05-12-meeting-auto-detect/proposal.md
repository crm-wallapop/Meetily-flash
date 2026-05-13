## Why

The user wants Meetily to start recording automatically when they join a Google Meet call, instead of remembering to click "Start Recording" each time. Manual triggering routinely costs the first few minutes of meetings (the user notices mid-call, scrambles to start). The user often joins calls mid-conversation, so even small detection delays sacrifice relevant context.

## What Changes

- **NEW**: A background detector polls Windows for an active Google Meet call using two combined signals: (1) a top-level window owned by Chrome/Edge/Firefox with title matching `* - Google Meet`, and (2) the same browser process has an active UDP/TCP connection to a known Google media-server IP range (i.e., a live Meet WebRTC connection). Detection is tab-agnostic — it answers "is the user in a call?" without identifying which tab. The WebRTC connection signal is Meet-specific by construction: it rejects browser music playback, dictation tools, Discord PWAs, and any other browser audio activity that isn't an actual Meet call.
- **NEW**: Optimistic title resolution gives the user a best-effort default that is editable and overridable. At the detection transition, the title is resolved in priority order: (1) the foreground window at that moment if it matches Meet, (2) the most recent Meet window from a continuous focus tracker (last 10 min), (3) the first Meet-titled window from `EnumWindows`, (4) a generic timestamp.
- **NEW**: On detection, Meetily begins recording immediately *and* shows a 10s countdown banner with an editable title field and a dropdown of all currently-detected Meet windows. If the user cancels, the recording is stopped and its audio + DB row are deleted atomically. If the user confirms or the timer expires, the title is committed and the recording continues normally.
- **NEW**: When the Meet WebRTC connection is gone for 10s, a `meeting-ended` event fires. A 10s prompt "Call ended — stop recording? [Stop now] [Keep recording]" gives the user an escape hatch; on timeout the recording stops via the normal save path. Signal re-engagement during the prompt (transient network drop healed) dismisses the prompt silently.
- **NEW**: A `cancel_recording` Tauri command stops a recording and atomically deletes its audio file and DB row — needed because the existing `stop_recording` finalises and saves.
- **NEW**: A startup GC pass reconciles orphan state from prior crashed/cancelled sessions: DB rows referencing missing audio files are deleted; audio files in the recordings directory not referenced by any meeting row are deleted. Both branches are logged.
- **NEW**: PWA support is transparent — Meet PWAs run inside the same browser process and are enumerated by `EnumWindows` like any other top-level window. No PWA-specific code is needed.
- **NEW**: Conservative app-start behaviour — if Meetily launches while a Meet call is already in progress, the detector does NOT fire (the connection didn't appear during the detector's observation window). User can start manually for in-progress calls.
- **NEW**: Cancel-suppression scope — a user who clicks Cancel during the countdown is not re-prompted for the same call. The suppression resets when the state machine transitions back to Idle.
- **NEW**: Concurrent-action precedence — manual "Start Recording" during the auto-start countdown silently cancels the auto-recording and starts a fresh manual one; manual always wins. Auto-detect fires while a manual recording is in progress are ignored.
- **NEW**: Settings toggle `auto_detect_meetings` (default: **on**). When off, the detector is not started and no events fire. Toggling at runtime is **v2 deferred work** — UI must communicate "Changes take effect after restart."

## Capabilities

### New Capabilities

- `meeting-auto-detect`: Windows-side detection of active Google Meet calls and the recording lifecycle that hangs off it (auto-start with countdown, auto-stop with countdown, cancel-with-cleanup, app-start state, GC reconciliation).

### Modified Capabilities

_(none — no existing spec files exist to delta against)_

## Impact

**Rust (Tauri):**
- `frontend/src-tauri/src/ports/meeting_detector.rs` (new) — `MeetingDetectorPort` trait and `DetectorObservation` value type
- `frontend/src-tauri/src/detection/mod.rs` (new) — module re-exports
- `frontend/src-tauri/src/detection/windows.rs` (new) — Windows adapter (Win32 EnumWindows + iphlpapi socket enumeration + foreground tracker)
- `frontend/src-tauri/src/detection/google_cidrs.rs` (new) — hardcoded Google media-server CIDR list
- `frontend/src-tauri/src/use_cases/meeting_detection.rs` (new) — polling loop + state machine + cancel-suppression
- `frontend/src-tauri/src/use_cases/recording_gc.rs` (new) — orphan DB row + audio file cleanup on startup
- `frontend/src-tauri/src/lib.rs` — wire up GC + detector on startup, register `cancel_recording`
- `frontend/src-tauri/src/audio/recording_manager.rs` — add `cancel_recording_and_cleanup` path
- `frontend/src-tauri/Cargo.toml` — add `windows` crate features for IpHelper / WinSock / WindowsAndMessaging / Threading

**TypeScript (frontend):**
- `frontend/src/components/AutoDetectBanner.tsx` (new) — countdown banner UI with editable title field and Meet-window dropdown
- `frontend/src/contexts/RecordingProvider.tsx` (or sidebar context) — listen to `meeting-detected` / `meeting-ended` events, route to banner, handle confirm/cancel/timeout, track detector-started recordings
- `frontend/src/components/Settings/` — `auto_detect_meetings` toggle with "Changes take effect after restart" help text

**Storage:** No schema changes. Existing meeting + audio cleanup paths are extended.

**Platform scope:** Windows only for v1. macOS adapter is a future change against the same `MeetingDetectorPort` trait.

**Browser scope:** Chrome (`chrome.exe`), Edge (`msedge.exe`), and Firefox (`firefox.exe`). The detection mechanism is process-name-agnostic above the allowlist; adding browsers is a one-line constant change.

**Deferred to v2 (tracked in design.md "Future Work" — to become GitHub issues at archive time):**
- Runtime toggle of `auto_detect_meetings` without app restart.
- macOS adapter implementing `MeetingDetectorPort`.
- Capture-peak fallback signal (documented in D19) — pivot path if smoke testing reveals connection-signal issues.
- Google CIDR auto-refresh from published ASN data.
- Detection for non-Meet platforms (Zoom, Teams, Slack Huddles).

**Security:** No new external input. Detection runs entirely on local OS APIs (Win32 + iphlpapi). The hardcoded CIDR list is data, not behaviour, and contains no secrets.
