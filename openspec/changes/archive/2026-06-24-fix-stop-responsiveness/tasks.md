## 1. Backend phase model

- [x] 1.1 Add `RecordingPhase` enum (`Idle` / `Recording` / `Saving`) in
      `recording_commands.rs` with `repr(u8)` and `From<u8>` / `Into<u8>` impls
- [x] 1.2 Replace the `IS_RECORDING: AtomicBool` static with
      `RECORDING_PHASE: AtomicU8`, default `Idle`; provide
      `current_phase()` / `set_phase(phase)` helpers that use `Ordering::SeqCst`
- [x] 1.3 Write a failing unit test that asserts `current_phase()` reads back
      the phase set by `set_phase()` for each variant (sanity check on enum
      round-trip); make it pass
- [x] 1.4 Write a failing unit test that asserts `set_phase(Recording)`
      followed by `set_phase(Saving)` followed by `set_phase(Idle)` is
      observable in that order from another task awaiting on a notify; make
      it pass

## 2. Phase-aware stop sequence (Rust)

- [x] 2.1 Write a failing integration test
      (`recording_commands_tests::stop_releases_streams_within_1s`) that
      spawns a recording, queues 10 dummy transcription chunks, calls
      `stop_recording`, and asserts the command returns within 1 second
- [x] 2.2 Write a failing integration test
      (`stop_emits_saving_phase_event`) that asserts the
      `recording-state-changed` event for `Saving` is emitted before
      `stop_recording` returns
- [x] 2.3 Refactor `stop_recording` so that the synchronous body is:
      (a) phase guard (return Ok if not `Recording`);
      (b) `stop_streams_and_force_flush().await`;
      (c) `set_phase(Saving)` + emit `recording-state-changed { Saving }`;
      (d) `tokio::spawn` the remaining shutdown work;
      (e) return Ok
- [x] 2.4 Move transcription drain, model unload, analytics emit, and file
      save into the spawned background task; on completion call
      `set_phase(Idle)` and emit `recording-state-changed { Idle }` plus the
      existing `recording-stopped` event for backwards-compat
- [x] 2.5 Make tests 2.1 and 2.2 pass

## 3. Idempotency and concurrent guards (Rust)

- [x] 3.1 Write a failing test
      (`stop_recording_is_idempotent_during_saving`) that calls
      `stop_recording` twice in quick succession with a slow background task
      and asserts the second call returns Ok without restarting any shutdown
      step
- [x] 3.2 Implement the phase-guard branches in `stop_recording`: return Ok
      early if phase is `Idle` or `Saving`; only proceed if `Recording`
- [x] 3.3 Make test 3.1 pass
- [x] 3.4 Write a failing test
      (`start_recording_rejected_during_saving`) that sets the phase to
      `Saving` and asserts `start_recording` returns an error mentioning
      "finalizing"
- [x] 3.5 Add the phase guard at the top of `start_recording`; make 3.4 pass

## 4. Background-shutdown failure handling (Rust)

- [x] 4.1 Write a failing test (`shutdown_failure_transitions_to_idle`) that
      forces the file-save step to error and asserts (a) the phase ends in
      `Idle` and (b) a `recording-save-failed` event is emitted with the
      error string
- [x] 4.2 Wrap each background-shutdown step in error capture; on any error
      log it, emit `recording-save-failed { error }`, and still transition
      to `Idle`
- [x] 4.3 Make test 4.1 pass

## 5. Tauri command surface

- [x] 5.1 Extend `get_recording_state` return type with a `phase: String`
      field; add corresponding TypeScript type in
      `frontend/src/types/recording.ts`
- [x] 5.2 Register `recording-state-changed` event payload type in the same
      file
- [x] 5.3 Run `cargo check` and `pnpm tsc --noEmit` to confirm both sides
      compile

## 6. Frontend state context

- [x] 6.1 Write a failing Vitest test for `RecordingStateContext` that
      simulates `recording-state-changed { phase: "Saving" }` and asserts
      `isRecording` becomes `false` AND `isSaving` becomes `true`
- [x] 6.2 Update `RecordingStateContext.tsx` to listen for
      `recording-state-changed`, derive `isRecording` / `isSaving` from
      the phase, and tear down the listener on unmount
- [x] 6.3 Keep the existing `recording-stopped` listener for the `Idle`
      transition (backwards-compat) — verify it doesn't double-fire
- [x] 6.4 Make test 6.1 pass

## 7. Frontend UI updates

- [x] 7.1 Add a Vitest test that renders `RecordingStatusBar` with
      `phase: Saving` and asserts:
      (a) no red recording dot,
      (b) a gray spinner is shown,
      (c) the label is "Saving…",
      (d) no Stop button is rendered
- [x] 7.2 Add a discriminated render branch in `RecordingStatusBar.tsx` for
      the `Saving` phase
- [x] 7.3 Remove the `isStopping` local-state guard in
      `RecordingControls.tsx` (the phase atomic now serves that role); the
      Stop button simply renders only when `isRecording` is true
- [x] 7.4 Make test 7.1 pass

## 8. Spec drift check and archive prep

- [x] 8.1 Re-read `openspec/specs/recording-lifecycle/spec.md`,
      `openspec/changes/fix-stop-responsiveness/design.md`, and the delta
      spec; confirm implementation matches every scenario and that no new
      observed behavior is missing from the spec
- [x] 8.2 Run `cargo test`, `pytest backend/`, `pnpm test`, `pnpm lint`;
      all green
- [x] 8.3 Manual smoke residue, closed on the automatable halves by binding
      specs (per the verify-with-existing-data principle — don't label
      "manual" what a spec already pins):
      **(a) Saving-phase render + Recording→Saving→Idle transition wiring** —
      `e2e/smoke/recording-basic.spec.ts` "stop transitions the status bar into
      Saving then clears on Idle" asserts the `saving-status-bar` / spinner /
      "Saving…" branch paints on `recording-state-changed(Saving)`, then clears
      on `Idle`. Branch CONTENT (no red dot, no Stop button) is the Vitest 7.1
      component test; the smoke pins only the event→context→render wiring.
      **(b) Auto-detect → leave → confirm-Stop → `stop_recording` consolidated
      call site (§9.5)** — `e2e/smoke/meeting-auto-detect.spec.ts` "stop-prompt
      confirm drives stop_recording and the Saving phase" emits meeting-detected
      → meeting-ended, clicks the stop-prompt "Stop Recording" button, and
      asserts `stop_recording` lands in the call log and Saving renders.
      **(c) Phase-machine timing** — cargo `stop_sync_path_transitions_phase_to_
      saving_and_returns_fast` asserts the synchronous stop path returns <1 s
      and leaves the phase in Saving.
      **(d) Real-adapter teardown timing + capture halt** —
      `cargo test -p meetily-flash --lib -- --ignored
      real_device_stop_releases_streams_within_1s_and_halts_capture`
      (recording_manager.rs) opens the real default input device, calls the
      real `stop_streams_and_force_flush()`, asserts it returns <1 s, and
      asserts `active_stream_count() == 0` after (zero streams ⇒ no cpal
      callback can deliver samples ⇒ capture halted). Content-independent
      (uses the stream count, not VAD-gated chunks), so it holds in silence.
      Verified green (0.46 s). This is the only way to measure real cpal
      teardown — the cargo phase-machine test (c) exercises a stub; this one
      drives the real adapter. See hexagonal-port-traits design D5 for why the
      use-case half is covered by port-trait unit tests while only this test
      covers the adapter half.
      **Residue that genuinely needs a live Meet call (not automatable today):**
      a real Meet call driving the real `WindowsMeetingDetector` — the
      latch/debounce LOGIC is cargo-tested; only the hardware constant
      (real WASAPI driving the `bc_drop` latch from a real browser) is
      un-pinned by this test, which covers the stop path, not detection.

## 9. Fix recording-stopped event timing race and consolidate stop call sites

Smoke test on 2026-05-13 revealed the audio.mp4 finalize step happened
~10 min after Stop press, but the frontend save flow ran immediately. The
meeting was saved to SQLite with `folder_path = null` because the
`recording-stopped` event (which delivered `folder_path` via sessionStorage)
fired from `background_shutdown` AFTER the save flow had already run.

Compounding bug: the auto-detect "stop-prompt banner" Confirm path called
`handleRecordingStop` directly without ever invoking `stop_recording`, so
the backend never received the stop signal at all from that code path.

- [x] 9.1 Add `StopRecordingResult { folder_path, meeting_name }` struct in
      `recording_commands.rs`; change `stop_recording` return type from
      `Result<(), String>` to `Result<StopRecordingResult, String>`
- [x] 9.2 Emit `recording-stopped` synchronously in `stop_recording` right
      after `set_phase(Saving)`, carrying the same folder_path/meeting_name
      that will be returned. Remove the duplicate emit at the end of
      `background_shutdown`. (`recording-saved` from `recording_saver.rs`
      remains the canonical "audio.mp4 finalized" signal.)
- [x] 9.3 Update `lib.rs::stop_recording` Tauri wrapper to forward the
      `StopRecordingResult`; drop the unused `save_path` arg and the dead
      directory-creation block. Remove the now-unused `RecordingArgs`
      struct in `lib.rs`.
- [x] 9.4 Add `StopRecordingResult` TypeScript type and `RecordingSavedPayload`
      to `recordingService.ts`. Change `stopRecording()` signature to take
      no args and return `Promise<StopRecordingResult>`. Add
      `onRecordingSaved()` listener helper for late-arriving audio refresh.
- [x] 9.5 Consolidate stop call sites: move `invoke('stop_recording')` into
      `handleRecordingStop` at the top, so both the manual Stop button
      (`RecordingControls.stopRecordingAction`) and the auto-detect banner
      (`useAutoDetect.handleBannerConfirm`) go through one call path.
      Remove the misleading "stop_recording is already called by
      RecordingControls" comment.
- [x] 9.6 In `handleRecordingStop`, read folder_path/meeting_name from the
      `StopRecordingResult` directly (with sessionStorage as fallback for
      tray-initiated stops that still emit `recording-stop-complete` to the
      frontend). Drop the `recordingStoppedDataRef` await — no longer
      needed once the value comes back from invoke.
- [x] 9.7 Simplify `RecordingControls.stopRecordingAction` to delegate to
      `onRecordingStop(true)`; drop `appDataDir` import, the unused
      `recordingPath`/`setRecordingPath` state, and the savePath
      construction.
- [x] 9.8 Run `cargo check` and `pnpm tsc --noEmit`; both green.
- [x] 9.9 Re-run the full test suite (`cargo test`, `pnpm test`); all green
      (13 Rust unit tests including 2 new struct-serialization regression
      tests; 59 TypeScript tests).
- [x] 9.10 Manual smoke residue, closed on the automatable halves by binding
       specs:
       **(a) `folder_path` delivered synchronously (not null)** — the §9.2 fix
       emits `recording-stopped` with `folder_path` before `stop_recording`
       returns, and `StopRecordingResult` carries it back via invoke. Pinned by
       the §9.9 struct-serialization regression tests + `e2e/smoke/recording-
       basic.spec.ts` (the mock returns `{ folder_path: '/tmp/smoke' }` and the
       meeting appears in the sidebar list — the original null-folder_path
       symptom can't recur without breaking these).
       **(b) Meeting appears in sidebar after Stop** — `recording-basic.spec.ts`
       test 1 asserts `stop_recording` pushes the meeting into `__smokeMeetings`
       and `api_get_meetings` is re-invoked after stop.
       **(c) Banner-confirm path also reaches the save** — the
       `meeting-auto-detect.spec.ts` §9.5 test asserts the stop-prompt confirm
       fires `stop_recording` (the path that previously bypassed the backend
       entirely).
       **Residue that genuinely needs a live mic (not automatable today):** (1)
       a real `audio.mp4` landing on the real filesystem after a real
       `background_shutdown` MP4 flush — the mock's `__smokeMeetings.push` stands
       in for the SQLite row but no real file is written; asserting prompt file
       finalization needs a real recording_saver cycle on real audio. (2) The
       meeting-details audio player playing real audio end-to-end — the
       `_meeting-details.ts` smoke helper exists but the player needs a real
       audio source. Both are real-filesystem/real-audio concerns, not wiring
       gaps.
