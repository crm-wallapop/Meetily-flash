## 1. Hexagonal scaffolding

- [x] 1.1 Create `frontend/src-tauri/src/ports/meeting_detector.rs` defining `trait MeetingDetectorPort` and the value type `DetectorObservation { meet_windows: Vec<MeetWindow>, has_meet_connection: bool, connection_first_seen_at: Option<Instant> }` plus `MeetWindow { hwnd_id: usize, pid: u32, title: String }`. Write a unit test asserting `DetectorObservation` derives `Clone + Debug + PartialEq`.
- [x] 1.2 Create `frontend/src-tauri/src/use_cases/meeting_detection.rs` with `spawn_detector<P: MeetingDetectorPort + Send + 'static>(port: P, app: AppHandle, poll_interval: Duration, settings: Settings) -> JoinHandle`. Pure Rust, no platform code.
- [x] 1.3 Write a `MockMeetingDetector` test double under `#[cfg(test)]` in `use_cases/meeting_detection.rs` whose `current_state()` is scriptable (returns from a `VecDeque<DetectorObservation>`).

## 2. State machine — TDD red tests first

- [x] 2.1 Red test: from `Idle`, when port returns observation with title match AND `has_meet_connection=true` AND `connection_first_seen_at` is during the observation window, emit one `meeting-detected` event with the resolved title.
- [x] 2.2 Red test: from `Idle`, when port returns observation where `has_meet_connection` was already true at the first poll (i.e., `connection_first_seen_at` matches detector start time), do NOT emit any event (app-start state, D15).
- [x] 2.3 Red test: from `InCall`, when port returns `has_meet_connection=false` for less than 10s before returning to `true`, no `meeting-ended` event is emitted (transient drop debounce).
- [x] 2.4 Red test: from `InCall`, when port returns `has_meet_connection=false` continuously for ≥10s, emit one `meeting-ended` event and transition to `Idle`.
- [x] 2.5 Red test: cancel-suppression — after the frontend invokes a cancel-suppression signal during `InCall`, the detector does NOT re-emit `meeting-detected` even if the connection briefly drops and re-appears within the same `InCall` session. The flag resets on transition to `Idle` (D16).
- [x] 2.6 Red test: rapid alternation `connection=true → false → true → false` within 10s does NOT emit `meeting-ended`.
- [x] 2.7 Red test: title match WITHOUT `has_meet_connection` (Meet tab open, user not joined) keeps the detector in `Idle`.
- [x] 2.8 Red test: `has_meet_connection=true` WITHOUT a title match (somehow Meet connection but no Meet window) keeps the detector in `Idle`.
- [x] 2.9 Implement the state machine in `spawn_detector` until 2.1–2.8 pass green.

## 3. Windows detection adapter — network + window enumeration

- [x] 3.1 Add the `windows` crate with required features to `frontend/src-tauri/Cargo.toml` under `[target.'cfg(target_os = "windows")'.dependencies]`:
      ```
      windows = { version = "0.58", features = [
          "Win32_UI_WindowsAndMessaging",
          "Win32_System_Threading",
          "Win32_NetworkManagement_IpHelper",
          "Win32_Networking_WinSock",
          "Win32_System_Com",
      ] }
      ```
- [x] 3.2 Create `frontend/src-tauri/src/detection/mod.rs` (re-exports) and `frontend/src-tauri/src/detection/google_cidrs.rs` defining `const GOOGLE_MEDIA_CIDRS: &[&str]` with the initial hardcoded list (per D18). Include 10-20 representative Google-owned IPv4 ranges plus a small set of IPv6 ranges. Document the refresh policy in a module-level doc comment.
- [x] 3.3 In `detection/google_cidrs.rs`, implement `is_in_google_cidrs(ip: IpAddr) -> bool` using a CIDR-match helper (either pull `ipnet = "2.9"` as a small dep, or implement a tiny prefix-match against parsed `Ipv4Net` / `Ipv6Net` values). Unit-test with known-Google and known-non-Google IPs.
- [x] 3.4 Create `frontend/src-tauri/src/detection/windows.rs` defining `pub struct WindowsMeetingDetector` and a constructor that initialises COM via `CoInitializeEx` once. Hold `BROWSER_PROCESSES: &[&str] = &["chrome.exe", "msedge.exe", "firefox.exe"]` as a module constant.
- [x] 3.5 Implement `enumerate_meet_windows() -> Vec<MeetWindow>` using `EnumWindows` + `GetWindowTextW` + `GetWindowThreadProcessId`. Filter by process name via `OpenProcess` + `QueryFullProcessImageNameW`. Apply suffix-anchored regex `r"\s-\sGoogle Meet$"`. Unit-test title parsing in isolation with sample title strings including the literal-text trap (`"Google Meet sync - Google Meet"` → meeting name `"Google Meet sync"`).
- [x] 3.6 Implement `has_meet_connection(browser_pids: &[u32]) -> bool` using `GetExtendedUdpTable(TCP_TABLE_OWNER_PID_ALL)` and `GetExtendedTcpTable(TCP_TABLE_OWNER_PID_ALL)`. For each row whose owning PID is in `browser_pids`, check the remote IP via `is_in_google_cidrs`. Return `true` on first match. Note: also enumerate IPv6 variants (`GetExtendedUdp6Table` / `GetExtendedTcp6Table`).
- [x] 3.7 Implement `MeetingDetectorPort for WindowsMeetingDetector`: track previous-poll `has_meet_connection` to compute `connection_first_seen_at` on the rising edge. The detector remembers its startup `Instant` to enforce app-start-state semantics (D15).
- [x] 3.8 Implement the foreground tracker for D10 title resolution: a separate 1Hz Tokio task maintaining `Arc<Mutex<VecDeque<(String, Instant)>>>` capped at 10 entries. On each tick: `GetForegroundWindow() → GetWindowTextW()`; if title matches Meet regex, push (title, now). Prune entries older than 10 minutes on each push.
- [x] 3.9 Expose `resolve_default_title(observation: &DetectorObservation, focus_tracker: &FocusTracker) -> String` implementing D10's priority order. Unit-test each level (foreground snapshot match, recent-focus match, first-enumerated, generic timestamp).
- [ ] 3.10 Write a manual smoke test under `#[ignore]`: runs the real detector for 60 seconds and prints state transitions. Documented run procedure in the test's doc-comment. The smoke test MUST explicitly verify Meet PWA detection: open the PWA, confirm window enumeration sees it, connection signal fires, title resolves.

## 4. Adversarial tests (CLAUDE.md §4)

- [x] 4.1 Test: window title containing `"Google Meet"` as user text (e.g., a tab titled `"Chat with team about Google Meet"`) — must NOT match. Suffix-anchored regex is the guard.
- [x] 4.2 Test: SQL/path-traversal-ish meeting titles like `"'; DROP TABLE meetings; -- - Google Meet"` and `"../../etc/passwd - Google Meet"` — title is treated as opaque text and passed through to the existing storage layer; no special parsing.
- [x] 4.3 Test: unicode meeting titles including emoji (`"📊 Q4 review - Google Meet"`), RTL text, and characters outside BMP — regex handles correctly, title is stored as-is.
- [x] 4.4 Test: detector spawned while a recording is already in progress emits the event but the frontend handler ignores it without double-starting (D17).
- [x] 4.5 Test: state-machine receives a port that panics on `current_state()` — `spawn_detector` does not crash the app; it logs the error and continues polling.
- [x] 4.6 Test (Spotify-desktop FP): mock port returns title match but `has_meet_connection=false` — detector stays Idle.
- [x] 4.7 Test (Discord-PWA FP): mock port returns title match AND `has_meet_connection=false` (because the Discord WebRTC connection is NOT to Google IPs — the underlying network enumeration in the real adapter correctly skips it) — detector stays Idle. In the unit test this is exercised by simulating the observation directly.
- [x] 4.8 Test (app-start mid-call): mock port's first poll returns `has_meet_connection=true` with `connection_first_seen_at = detector_start_time` — detector does NOT emit `meeting-detected`.
- [x] 4.9 Test (cancel-suppression): scripted sequence — detector emits `meeting-detected`, frontend signals cancel, connection drops briefly (8s, less than debounce), connection returns. Detector does NOT re-emit `meeting-detected`. Then connection drops for 12s (exceeds debounce → Idle transition), then returns. Detector DOES re-emit (flag was reset on Idle transition).

## 5. cancel_recording command

- [x] 5.1 Red test (Rust): write a failing test in `audio/recording_manager.rs` for `cancel_recording(meeting_id)` asserting: stops audio capture, deletes audio file, deletes DB row, returns Ok with meeting_id.
- [x] 5.2 Red test: when DB deletion fails after file deletion succeeds, the function returns an error including both the meeting_id and the path so log readers and the GC pass can act.
- [x] 5.3 Implement `cancel_recording` until 5.1 and 5.2 pass.
- [x] 5.4 Register `cancel_recording` as a Tauri command in `frontend/src-tauri/src/lib.rs` (alongside `stop_recording`). Add to `invoke_handler`.

## 6. Startup GC pass

- [x] 6.1 Create `frontend/src-tauri/src/use_cases/recording_gc.rs` exposing `run_startup_gc(db: &DatabaseManager, recordings_dir: &Path) -> GcReport` where `GcReport { orphan_rows_deleted: usize, orphan_files_deleted: usize, errors: Vec<String> }`.
- [x] 6.2 Red test: orphan DB row (meeting row points to non-existent file) → GC deletes the row, returns count 1.
- [x] 6.3 Red test: orphan file (no meeting row references it) → GC deletes the file, returns count 1.
- [x] 6.4 Red test: valid meeting + valid file → GC touches neither, returns counts 0.
- [x] 6.5 Red test: partial failure (e.g., file is locked) → error is recorded in `GcReport.errors`, GC continues with remaining items.
- [x] 6.6 Implement `run_startup_gc` until 6.2–6.5 pass.
- [x] 6.7 Wire `run_startup_gc` into `lib.rs::run()` startup BEFORE the detector is spawned. Log the report; do NOT fail startup on errors.

## 7. Frontend wiring

- [x] 7.1 Create `frontend/src/components/AutoDetectBanner.tsx`: a controlled component with props `{ mode: 'detect-prompt' | 'stop-prompt', initialTitle: string, candidateTitles: string[], onConfirm: (title) => void, onCancel: () => void, timeoutSeconds: number }`. Internal countdown + visual indicator. Editable title field (only meaningful in `detect-prompt` mode). Dropdown of `candidateTitles` showing all detected Meet windows. The committed title is whatever is in the field when the user confirms or the timer expires.
- [x] 7.2 In the existing recording context (SidebarProvider or equivalent — discover the current architecture), add a `useEffect` that listens for `meeting-detected`. On event payload `{ default_title, candidate_titles }`: if not already recording, invoke `start_recording_with_devices_and_meeting` with `default_title`, then render `<AutoDetectBanner mode="detect-prompt" ... />`. On Cancel: invoke `cancel_recording`. On Confirm or timeout: invoke a small `update_meeting_title(meeting_id, new_title)` Tauri command if the committed title differs from the default.
- [x] 7.3 Same context: listen for `meeting-ended`. If a detector-started recording is active, show `<AutoDetectBanner mode="stop-prompt" ... />`. On Stop or timeout: invoke `stop_recording`. On "Keep recording": dismiss banner, mark recording as user-managed (no further auto-stop prompts).
- [x] 7.4 In the same context, listen for `meeting-detected` ALSO during an active stop-prompt: if received, dismiss the stop-prompt silently (signals re-engaged within debounce window — D17).
- [x] 7.5 Track whether the current recording was started by the detector vs. manually. Auto-stop prompt only fires for detector-started recordings. Manual "Start Recording" click during an active auto-start countdown silently cancels the auto-recording and starts a fresh manual one (D17).
- [x] 7.6 Add a small `update_meeting_title(meeting_id, new_title)` Tauri command (and Rust handler that updates the DB row). Used by the banner to commit edited titles.

## 8. Settings

- [x] 8.1 Add `auto_detect_meetings: bool` (default `true`) to the existing settings store. Read it in `lib.rs::run()` startup.
- [x] 8.2 If `auto_detect_meetings` is true, construct `WindowsMeetingDetector` and call `spawn_detector(...)`. Hold the `JoinHandle` in app state so it can be inspected. If false, log "auto-detect disabled" and skip.
- [x] 8.3 Add UI control: a toggle in the settings panel labelled "Auto-detect Google Meet calls". Save on change. Show inline help text: "Changes take effect after restart." Find the existing settings boolean pattern by grep before implementing.

## 9. Integration verification

- [x] 9.1 Run `cargo test --features vulkan` — all new unit tests pass alongside the existing suite.
- [x] 9.2 Run `cargo check --features vulkan` — release-build clean.
- [x] 9.3 Run `pnpm lint` — frontend changes pass lint.
- [ ] 9.4 Manual smoke test: with `auto_detect_meetings=true`, join a real Google Meet call from a Chrome browser tab. Verify banner appears within ~10s, recording is created, countdown completes, recording continues. Leave the call; verify stop-prompt appears and recording is saved.
- [ ] 9.5 Manual smoke test (PWA): repeat 9.4 using the Google Meet Chrome PWA. Verify all four steps work identically.
- [ ] 9.6 Manual smoke test (muted join): join a Meet call with mic muted before clicking Join. Verify the detector still fires within ~10s (relies on the WebRTC connection signal, not on mic state).
- [ ] 9.7 Manual smoke test (cancel cleanup): trigger detection, click Cancel during countdown. Verify the audio file is absent from disk AND the meeting does not appear in the meetings list.
- [ ] 9.8 Manual smoke test (false-positive resistance): play music in a browser tab while a Meet tab is open but you are NOT joined. Verify the detector does not fire. Repeat with browser-based dictation tools.
- [ ] 9.9 Manual smoke test (app-start state): join a Meet call BEFORE launching Meetily. Launch Meetily. Verify the detector does NOT fire for the in-progress call. Stop and restart the call (leave then rejoin); verify detection fires correctly for the rejoin.
- [ ] 9.10 Manual smoke test (orphan GC): manually create an orphan file in the recordings dir and an orphan DB row pointing to a missing file. Launch Meetily. Verify both are cleaned up and the log records both deletions.
- [ ] 9.11 Inspect logs after a normal call+record+stop cycle for any unexpected errors or warnings.
