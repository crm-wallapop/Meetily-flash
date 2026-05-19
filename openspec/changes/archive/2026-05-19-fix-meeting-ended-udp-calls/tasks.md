## 1. Cargo.toml — add Win32_Media_Audio feature

- [x] 1.1 Add `"Win32_Media_Audio"` to the `windows` crate features in `frontend/src-tauri/Cargo.toml` under `[target.'cfg(target_os = "windows")'.dependencies]`

## 2. Browser allowlist update

- [x] 2.1 Add `"brave.exe"` to `BROWSER_PROCESSES` in `detection/windows.rs`

## 3. WASAPI capture session function

- [x] 3.1 Write failing tests for `has_browser_capture_session()`: (a) smoke test — function returns a `bool` without panicking in the test environment (no browser with mic active in CI); (b) adversarial: COM init failure path returns `false`; (c) adversarial: empty session list returns `false`
- [x] 3.2 Implement per-call COM initialization in `has_browser_capture_session()`: call `CoInitializeEx(None, COINIT_APARTMENTTHREADED)`, check result — `S_OK` means we initialised (must call `CoUninitialize` after), `S_FALSE` means already initialised on this thread (must NOT call `CoUninitialize`), negative `HRESULT` means failure (return `false` immediately). Note: the detection loop is `tokio::spawn` (async, not `spawn_blocking`), so the future can run on different threads between polls — per-call init is the correct pattern.
- [x] 3.3 Implement `check_browser_capture_session_inner() -> windows::core::Result<bool>`: create `IMMDeviceEnumerator` via `CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)`, call `EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)` to get all active capture devices (not just `GetDefaultAudioEndpoint` — this misses sessions on non-default mics and `eCommunications` devices which Chrome uses for WebRTC). Iterate via `collection.GetCount()?` and `collection.Item(i)?` in a `for i in 0..count` loop — `IMMDeviceCollection` does not implement `Iterator`. For each device: activate `IAudioSessionManager2`, enumerate sessions via `GetSessionEnumerator`, for each session: check `GetState() == AudioSessionStateActive`, get PID via `cast::<IAudioSessionControl2>()?.GetProcessId()`, check `is_browser_process(pid)`, return `Ok(true)` on first match. Do NOT call `IsSystemSoundsSession` — it returns `S_FALSE` for non-system sessions which `is_ok()` in windows-rs causes every session to be skipped; `is_browser_process` already excludes system sessions by name.
- [x] 3.4 Implement public `has_browser_capture_session() -> bool`: call per-call COM init (3.2), on success call `check_browser_capture_session_inner().unwrap_or(false)`, unconditionally call `CoUninitialize` if `S_OK` was returned from init
- [x] 3.5 Verify all three tests from 3.1 pass

## 4. `notify_exit()` port method

- [x] 4.1 Add `fn notify_exit(&mut self) {}` with a default no-op body to `MeetingDetectorPort` in `ports/meeting_detector.rs`
- [x] 4.2 Implement `notify_exit(&mut self)` on `WindowsMeetingDetector` in `detection/windows.rs`: set `self.turn_established = false`
- [x] 4.3 In `spawn_detector` (`use_cases/meeting_detection.rs`), call `port.notify_exit()` immediately after `emitter.emit_ended()` in the `DetectorEvent::MeetingEnded` arm

## 5. Exit detection logic

- [x] 5.0 Add a `#[cfg(test)]` injectable probe struct to `WindowsMeetingDetector` so adapter-layer tests can control the return values of the three free functions without calling real Win32 APIs. Exact requirements for this task:
  - Bounds must include `+ Send + Sync` on all three trait objects — `spawn_detector` requires `P: MeetingDetectorPort + Send + 'static`, so without `Send + Sync` the detector becomes `!Send` in test builds and existing call sites fail to compile:
    ```rust
    #[cfg(test)]
    pub(crate) struct DetectorProbes {
        pub has_turn: Box<dyn Fn() -> bool + Send + Sync>,
        pub has_conn: Box<dyn Fn() -> bool + Send + Sync>,
        pub has_capture: Box<dyn Fn() -> bool + Send + Sync>,
    }
    ```
  - The `probes` field on the struct and all access sites in `current_state()` must be cfg-gated so release builds compile cleanly. Pattern:
    ```rust
    // in the struct
    #[cfg(test)]
    pub(crate) probes: Option<DetectorProbes>,

    // in current_state()
    #[cfg(test)]
    let turn = self.probes.as_ref().map_or_else(has_turn_connection, |p| (p.has_turn)());
    #[cfg(not(test))]
    let turn = has_turn_connection();
    // (same for has_conn and has_capture)

    // in new()
    #[cfg(test)]
    probes: None,
    ```
  - Add a `with_probes(probes: DetectorProbes) -> Self` constructor in a `#[cfg(test)]` impl block.
  - Note: a fourth `meet_windows` probe was added beyond the task spec because `current_state()` branches on `meet_windows.is_empty()` before reaching the TURN/WASAPI logic; without injecting non-empty windows, tests (b) and (c) would always hit the early-return path.

- [x] 5.1 Write failing tests at two levels:
  - **`step_detector` / mock layer** (a): script a `MockMeetingDetector` sequence where `has_meet_connection` drops to `false` for 10 s — verify `meeting-ended` is emitted. This tests the debounce path without needing to observe internal adapter state.
  - **`WindowsMeetingDetector` adapter layer** (b) and (c): write unit tests directly on `WindowsMeetingDetector::current_state()` and `notify_exit()`, not on `step_detector` or `MockMeetingDetector` — the TURN/WASAPI logic and `turn_established` flag are internal to the adapter and invisible through the mock surface. (b) Set `turn_established = true` on a new detector, call `notify_exit()`, then call `current_state()` with a mocked environment where WASAPI returns true — verify `has_conn = true` (i.e., next UDP call is detectable after exit). (c) Otter.ai scenario: `turn_established = true`, call `current_state()` with WASAPI true and TURN false for multiple iterations — verify `has_conn = false` each poll (debounce protected); then call `notify_exit()`, then `current_state()` with WASAPI true — verify `has_conn = true`.
- [x] 5.2 In `current_state()`, update the `!turn_established` else branch: replace `let conn = has_meet_connection(); conn` with `let conn = has_meet_connection() && has_browser_capture_session(); conn` (update the existing `log::debug!` message to reflect the two-signal check)
- [x] 5.3 Verify all three tests from 5.1 pass; verify all existing detector tests still pass

## 6. Spec update — living meeting-auto-detect spec

- [x] 6.1 Apply the delta spec to `openspec/specs/meeting-auto-detect/spec.md`: replace the "Detect when an active call ends" requirement body and scenarios with the updated two-signal text from the delta spec; replace the "Meeting detection gates the transcription queue" known-bug note with the fixed-2026-05-18 note. Also document the `notify_exit()` addition to `MeetingDetectorPort` — either add a sentence to the relevant requirement or add a brief note under "Detect when an active call ends" describing that the adapter's exit bookkeeping is reset via this callback.

## 7. Validation

- [x] 7.1 Run `cargo test --lib` and confirm all detection tests pass with no regressions
- [x] 7.2 Manual smoke test (join detection): open Meet in Edge → click "Join now" → confirm `meeting-detected` fires and recording starts within ~4 s
- [x] 7.3 Manual smoke test (exit detection): while in the call from 7.2 → click "Leave call" → confirm `meeting-ended` fires within ~20 s and the auto-stop banner appears (WASAPI drops ~2 s after leave, then 15 s debounce = ~17 s total; observe no false-fire during the call if mic was briefly inactive)
- [x] 7.4 Verify "getUserMedia open when muted" assumption: while in a Meet call in Edge with mic muted, open Windows Sound settings → Recording tab → confirm the Edge capture session is visible and active. If absent, document in design.md Open Questions — the fix still works but false `meeting-ended` may fire after 10 s of continuous muting.
