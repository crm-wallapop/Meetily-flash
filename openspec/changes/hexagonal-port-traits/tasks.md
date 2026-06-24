## 1. Port trait + fake (the seam)

- [ ] 1.1 Define `AudioCapturePort` in `ports/audio_capture.rs` (an `async fn
      start` returning a chunk receiver + `async fn stop_streams_and_flush`),
      and register the module in `ports/mod.rs`. Resolve Open Question 1
      (receiver type — lean concrete `mpsc::UnboundedReceiver<AudioChunk>` per
      KISS) and Open Question 2 (include `start` on the port so the fake owns
      the full lifecycle — yes). Follow the `MeetingDetectorPort` precedent for
      shape.
- [ ] 1.2 Write a failing test: `AudioCaptureFake` (instant
      `stop_streams_and_flush` + a chunk counter over a channel the test holds
      the sender of) implements `AudioCapturePort`; calling stop records the
      invocation. (Fails today: the trait + fake do not exist.)
- [ ] 1.3 Make 1.2 pass.

## 2. Pure stop use case (the extraction)

- [ ] 2.1 Write a failing test `stop_use_case_calls_port_and_flips_phase`:
      `use_cases::recording_lifecycle::stop_recording(&mut dyn
      AudioCapturePort, &phase_state)` calls `stop_streams_and_flush`, then
      sets the phase to `Saving`; the use case references no concrete
      `RecordingManager`. (Fails today: the use case does not exist.)
- [ ] 2.2 Implement the pure use case by lifting the synchronous stop body out
      of `recording_commands.rs::stop_recording` (phase guard → port
      `stop_streams_and_flush` → `set_phase(Saving)` → `tokio::spawn`
      background shutdown). No concrete adapter import inside the use case.
- [ ] 2.3 Make 2.1 pass.

## 3. RED — adversarial gap-closing tests (the point of the change)

These two tests are the permanent guards for the stop-responsiveness
guarantees that were previously only empirically confirmed. Per design D5
they prove the **use-case half**; the real-adapter half stays `#[ignore]`.

- [ ] 3.1 Write a failing test `stop_phase_flips_within_1s_with_instant_fake`:
      inject the fake (instant stop), call the use case, assert it returns and
      the phase is `Saving` within 1 s, with zero device I/O performed. This is
      the cargo gate for the "status bar clears within 1 s" guarantee's
      use-case latency.
- [ ] 3.2 Write a failing test `no_chunks_after_stop`: inject the fake with a
      chunk counter, call the use case, and after `stop_streams_and_flush`
      returns assert zero chunks arrive on the receiver over a sampling
      window. This is the cargo gate for the "audio capture halts within 1 s"
      guarantee at the seam.
- [ ] 3.3 Make 3.1 and 3.2 pass (they should pass once §2 lands; a failure
      means the use case adds latency or fails to gate the receiver).

## 4. Real adapter implements the trait (production wiring)

- [ ] 4.1 `impl AudioCapturePort for RecordingManager`, delegating `start` and
      `stop_streams_and_flush` to the existing methods. The manager keeps all
      ~20 fields and internals (facade per design D2).
- [ ] 4.2 Refactor `stop_recording_sync_path_for_test()` to drive the
      port-backed use case with the fake. This resolves the test's existing
      honesty caveat ("the real stop path is not measured here") for the
      use-case half — the use case IS now measured; only real cpal teardown
      remains `#[ignore]`-confirmed (design D6).
- [ ] 4.3 Confirm the existing phase-machine test
      `stop_sync_path_transitions_phase_to_saving_and_returns_fast` still
      passes, now exercising the real use case against the fake rather than a
      divorced stub.

## 5. Composition root + Tauri command (DI)

- [ ] 5.1 In `lib.rs`, construct ONE `Arc<RecordingManager>`, clone + upcast
      into `Arc<dyn AudioCapturePort + Send + Sync>`, and register it in Tauri
      app state. Sole cross-boundary import per §2a; no second manager is ever
      constructed (design D3 + Risk mitigation).
- [ ] 5.2 Refactor `recording_commands.rs::stop_recording` to resolve the port
      from state and delegate to the pure use case. Command signature is
      unchanged — same args, still returns `StopRecordingResult`, still
      idempotent during `Saving`.
- [ ] 5.3 `cargo check` green.

## 6. Verify — no behavioral regression

- [ ] 6.1 `cargo test` full crate green — existing phase-machine, idempotency,
      and start-during-saving tests unchanged, plus the new §3 gap tests.
- [ ] 6.2 `pnpm test:smoke` green — `recording-basic` (Saving-phase render) and
      `meeting-auto-detect` (§9.5 consolidated stop) hold the behavior. This
      change adds no new smoke spec because it touches no user-visible frontend
      behavior (CLAUDE.md §3 carve-out); the existing specs are the regression
      net called out in Risks.
- [ ] 6.3 Re-read `openspec/specs/recording-lifecycle/spec.md` and this
      `design.md`; confirm the 1 s bound, `Saving` phase, `StopRecordingResult`,
      and idempotency are all still accurately described after the refactor
      (gates don't catch spec drift — read the spec, not just the diff).
- [ ] 6.4 Note the unblocked follow-ons in the archive: the §4 capture-seam
      scenarios (device-disconnect mid-recording, permission-denied, sample-rate
      mismatch) and `cargo-integration-test-depth` are now possible as separate
      changes; the real-cpal-timing confirmation is the `#[ignore]` real-device
      test (Option 1), implemented alongside but tracked separately per design
      D5.
