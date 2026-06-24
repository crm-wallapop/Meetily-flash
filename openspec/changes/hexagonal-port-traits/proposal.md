> **STATUS: DEFERRED 2026-06-24.** Both stop-responsiveness guarantees are
> already covered — G1 (status bar clears <1s) by the phase-machine cargo tests,
> G2 (capture halts / no chunks after stop) by the `#[ignore]` real-device test
> `real_device_stop_releases_streams_within_1s_and_halts_capture` (merged
> 2026-06-24). The Why/What below were written against incorrect architecture
> premises (the flush is in `background_shutdown`, not the sync path; the manager
> is a global static consumed on stop, not app state) — see `design.md`
> § "DEFERRED & architecture corrections" for the real architecture, the deferral
> reasoning, and the corrected Option 2 implementation path. Revisit when a §4
> adversarial need (device-disconnect / permission-denied / sample-rate-mismatch)
> demands a swappable capture port.

## Why

The recording stop path can't be unit-tested. `RecordingManager`
(`audio/recording_manager.rs`) fuses three roles behind one struct: the **adapter**
(owns cpal streams + the incremental saver), the **use case** (start/stop/flush
lifecycle, phase transitions), and **state** (meeting id, phase, pause, folder
path, ~20 fields). Because the lifecycle logic calls `stop_streams_and_force_flush()`
on a concrete cpal adapter inline, there is no seam to inject a fake capture — so
the two stop-responsiveness guarantees that are NOT already covered (the 1 s
real-stream-teardown timing, and "no audio captured after Stop") can only be
exercised against a live microphone. This is the same blocker, called out in
CLAUDE.md §8, that keeps the entire §4 adversarial surface (device-disconnect,
permission-denied, sample-rate-mismatch, LLM-timeout/malformed) from cargo
integration-depth testing.

Why now: the `fix-stop-responsiveness` change just hardened the stop path's
behavior and strengthened its smoke coverage. The next step is to make the
guarantees on that path *automatically verifiable* — not just empirically
confirmed once — so a future refactor can't silently regress the 1 s bound or
reintroduce post-stop audio capture.

## What Changes

- Introduce `ports/audio_capture.rs` with an `AudioCapturePort` trait covering
  the recording lifecycle seam: `start` (returns a chunk receiver) and
  `stop_streams_and_flush` (releases cpal streams + force-flushes the saver).
- Peel the **stop path** off `RecordingManager` into a pure
  `use_cases/recording_lifecycle` use case that consumes the port trait, sets
  the phase, and spawns the background shutdown. The pure use case takes the
  port + phase state, returns the result — no concrete adapter inside.
- `RecordingManager` becomes the real cpal **adapter** (impls the trait); a new
  test fake (`AudioCaptureFake`) impls it with instant stop + a chunk counter.
- The `stop_recording` Tauri command (and the synchronous test stub
  `stop_recording_sync_path_for_test`) is refactored to drive the port-backed
  use case. The port reaches the command via app state (`Arc<dyn
  AudioCapturePort>`), since Tauri command handlers need concrete (non-generic)
  arg types.
- **Scope is deliberately minimal**: only the stop-path lifecycle seam gets a
  port. Full §2a decomposition (transcriber, llm, storage, the whole pipeline)
  is deferred to follow-on changes per adapter, once the pattern is proven on
  the highest-value seam. `domain/` is not introduced by this change (the
  RecordingPhase value object already lives where it's used).

Non-goal: no user-visible behavior changes. The 1 s bound, the Saving phase, the
`StopRecordingResult` contract, and idempotency all behave exactly as today —
this change makes them *testable without a live mic*, it doesn't alter them.

## Capabilities

### New Capabilities
<!-- none — this is architecture/testability work on an existing capability -->

### Modified Capabilities
- `recording-lifecycle`: add a requirement that the stop path SHALL consume a
  swappable `AudioCapturePort` (trait, not concrete adapter), so the 1 s
  stream-release timing and the no-audio-after-stop invariant are verifiable
  via a fake capture in cargo tests without a live device.

## Impact

- **Rust ports**: `frontend/src-tauri/src/ports/audio_capture.rs` (new trait +
  value objects for the start config / chunk receiver contract).
- **Rust use cases**: `frontend/src-tauri/src/use_cases/recording_lifecycle.rs`
  (new — the pure stop use case extracted from the Tauri command body).
- **Rust adapter**: `audio/recording_manager.rs` impls `AudioCapturePort`; the
  lifecycle methods it exposes narrow to the trait surface. `audio/
  recording_commands.rs::stop_recording` delegates to the use case.
- **Composition root**: `lib.rs` wires `Arc<dyn AudioCapturePort>` into app
  state; the command handler resolves it. The `detection/fake.rs` precedent
  (FakeMeetingDetector) is the template.
- **Tests**: unlocks (a) the two stop-responsiveness gap tests as millisecond
  unit tests (fake port — instant stop proves phase flips fast; chunk counter
  proves no capture after stop), and (b) the prerequisite for
  `cargo-integration-test-depth` (device-disconnect, permission-denied,
  sample-rate-mismatch on the capture seam).
- **No frontend / DB / API impact.** No breaking changes to Tauri command
  signatures (the command still takes the same args and returns
  `StopRecordingResult`).
- **Regression risk**: the stop path is where the 2-minute-lag and
  `folder_path = null` bugs lived. The strengthened smoke suite
  (`recording-basic` Saving-phase test, `meeting-auto-detect` §9.5 test) and
  the existing cargo phase-machine tests are the safety net — that coverage was
  deliberately landed first.
