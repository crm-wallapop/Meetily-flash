## Context

The detector side of the Tauri app already proves the target pattern:
`MeetingDetectorPort` (`ports/meeting_detector.rs`) separates a pure value
object (`DetectorObservation`) from the adapter trait; the pure
`step_detector` use case consumes the port; `WindowsMeetingDetector` and
`FakeMeetingDetector` both implement it. That is why the detector-turn-latch
and `stable_capture` work was cargo-testable without a live Meet call.

The recording side has no such seam. `RecordingManager`
(`audio/recording_manager.rs`) is a ~20-field struct that fuses adapter (cpal
streams + incremental saver), use case (start/stop/flush lifecycle, phase
transitions), and state (meeting id, phase, pause, folder path). The
`stop_recording` Tauri command body (`audio/recording_commands.rs`, ~line 714)
calls `manager.stop_streams_and_force_flush().await` inline — a concrete cpal
adapter call with no trait boundary. So the two stop-responsiveness guarantees
not already covered (the 1 s real-stream-teardown timing, and "no audio
captured after Stop") can only run against a live microphone, and the §4
adversarial surface (device-disconnect, permission-denied, sample-rate-mismatch)
is blocked at the same seam (CLAUDE.md §8).

Hexagonal boundaries per CLAUDE.md §2a: `ports/` depends on `domain/` types
only; `use_cases/` depends on `ports/` traits; adapters depend on `ports/` +
native deps; `lib.rs` is the sole cross-boundary importer.

## Goals / Non-Goals

**Goals:**
- A swappable `AudioCapturePort` trait covering the stop-path lifecycle seam
  (start → chunk receiver; stop_streams_and_flush).
- The `stop_recording` use case extracted as a pure consumer of the port +
  phase state — no concrete adapter reference inside.
- A test fake (`AudioCaptureFake`) with instant stop + a chunk counter, enabling
  millisecond cargo unit tests for the two uncovered stop-responsiveness
  guarantees.
- Production behavior unchanged: the real cpal adapter (`RecordingManager`)
  implements the trait and is wired in the composition root; the 1 s bound,
  `Saving` phase, `StopRecordingResult`, and idempotency all hold as today.

**Non-Goals:**
- Full §2a port decomposition (transcriber, llm, storage, the whole pipeline).
  Follow-on changes per adapter, once the pattern is proven here.
- Introducing a `domain/` directory. `RecordingPhase` stays where it lives
  today; this change adds a port + use case, not a domain layer.
- Refactoring the audio *pipeline* (mic/system mixing, VAD, saver internals).
  The port boundary is the lifecycle seam only — start/stop/flush — not the
  chunk-processing topology.
- Changing any Tauri command signature, frontend contract, or DB schema.
- Proving the real cpal adapter's stream-Drop is fast. A fake port returns
  instantly by definition; confirming real-hardware timing is the job of the
  complementary `#[ignore]` real-device test (Decision D5).

## Decisions

### D1: Minimal stop-path port, not the full §2a layer

The trait covers only the lifecycle seam the stop path needs:
`start` (returns a chunk receiver) and `stop_streams_and_flush` (release cpal
streams + force-flush the saver). Transcriber/llm/storage ports are deferred.

**Why minimal:** the stop path is the highest-value seam — it's where the
2-minute-lag and `folder_path = null` bugs lived, and it's the seam the two
uncovered guarantees depend on. Proving the pattern here de-risks the broader
decomposition. A full §2a change in one shot would touch every adapter and the
composition root simultaneously, with the stop path's regression risk stacked
on top.

**Alternative considered:** extract all four §2a ports (audio_capture +
transcriber + llm + storage) in this change. Rejected — scope and regression
risk are disproportionate to the immediate goal, and the per-adapter follow-on
changes are independently valuable (each unblocks its own §4 categories).

### D2: RecordingManager becomes a trait-impling facade, not a decomposition

`RecordingManager` keeps its ~20 fields and internals; it gains an
`impl AudioCapturePort for RecordingManager` exposing the narrow trait surface.
The extracted `use_cases/recording_lifecycle::stop_recording` use case takes
`&dyn AudioCapturePort` (+ phase state) and contains the lifecycle logic
currently inline in the Tauri command body.

**Why not decompose RecordingManager now:** its fields are tightly coupled
(stream handles, the saver, gain sender, meeting state) and untangling them is
a separate, larger refactor. A facade — the manager impls the trait, the use
case consumes the trait — captures the testability benefit (the seam) without
the full decomposition risk. The detector precedent followed the same shape:
`WindowsMeetingDetector` kept its internals, it just implemented the port.

**Alternative considered:** split RecordingManager into AudioCaptureAdapter +
RecordingState + lifecycle orchestrator now. Rejected — YAGNI for this change;
the facade delivers the seam, and a future `recording-manager-decomposition`
change can split the struct without re-litigating the port boundary.

### D3: DI via `Arc<dyn AudioCapturePort>` in app state

Tauri command handlers deserialize args into concrete types, so the command
cannot be generic over `P: AudioCapturePort`. The port is stored in Tauri app
state as `Arc<dyn AudioCapturePort>`; the `stop_recording` handler resolves it
from state and passes it to the pure use case. `lib.rs` (the composition root)
constructs the real `RecordingManager`, upcasts it to `Arc<dyn
AudioCapturePort>`, and registers it in state — the sole cross-boundary import,
per §2a.

**Alternative considered:** a generic command handler `stop_recording<P>`.
Rejected — Tauri's `generate_handler!` macro needs concrete function signatures.

### D4: Chunk-receiver contract enables the no-chunks-after-stop test

`AudioCapturePort::start` returns a chunk receiver
(`mpsc::UnboundedReceiver<AudioChunk>` or equivalent). The fake's receiver is
backed by a channel the test holds the sender side of. The no-chunks-after-stop
test calls `stop_streams_and_flush`, then asserts the receiver yields no
further chunks over a sampling window (the fake stops sending on stop). This is
the same channel topology production uses — the fake just controls the sender.

### D5: The port proves the logic; a real-device test still proves the adapter

A fake port returns instantly, so it proves the *use case* adds no latency
beyond the adapter call and that the no-chunks invariant holds at the seam. It
**cannot** prove the real cpal adapter's stream-Drop is fast — that is adapter
behavior, outside the use case. The complementary `#[ignore]` real-device test
(opening a real capture stream, calling the real stop path, timing it) remains
the only way to confirm real-hardware teardown timing. The two are
**complementary, not redundant**: the port-trait unit test is the permanent
gate (runs every `cargo test`, catches use-case regressions); the `#[ignore]`
test is the periodic empirical confirmation that the real adapter behaves the
way the logic assumes. This is why both are being done — the proposal here is
the permanent guard; the `#[ignore]` test (Option 1, implemented after this
change) is the hardware confirmation.

### D6: RecordingPhase and the synchronous test stub

`RecordingPhase` / `RECORDING_PHASE` / `current_phase()` / `set_phase()` stay
where they are (the phase-machine tests depend on them). The existing
`stop_recording_sync_path_for_test()` stub is refactored to drive the
port-backed use case with the fake port — so the phase-machine timing test
(`stop_sync_path_transitions_phase_to_saving_and_returns_fast`) now exercises
the real use case against the fake, not a stub divorced from it. Its honesty
caveat ("the real stop path is not measured here") is resolved for the use-case
half: the use case IS measured; only the real-adapter half remains
#[ignore]-confirmed.

## Risks / Trade-offs

- **[Stop-path behavioral regression]** The stop path is where the 2-minute lag
  and `folder_path = null` bugs lived. → Mitigation: the strengthened smoke
  suite (`recording-basic` Saving-phase test, `meeting-auto-detect` §9.5
  consolidated-stop test) and the existing cargo phase-machine tests pin the
  behavior; they were deliberately landed first. The refactor must keep all of
  them green.
- **[cpal Send / thread affinity]** `RecordingManager` is `unsafe impl Send`;
  cpal stream handles have platform thread affinity. → Mitigation: the trait
  uses `async fn` so the real adapter's `stop_streams_and_flush` can hop to the
  correct thread internally; the fake is trivially `Send + Sync`. The
  `Arc<dyn AudioCapturePort + Send + Sync>` bound matches the existing
  detector-port wiring.
- **[Tauri state upcasting]** Storing `Arc<dyn AudioCapturePort>` alongside the
  existing concrete `RecordingManager` state could create two access paths to
  the same manager. → Mitigation: the composition root owns ONE
  `Arc<RecordingManager>`; it is cloned and upcast into the port slot once. The
  command resolves the port; nothing else constructs a second manager.
- **[Facade drift]** Leaving RecordingManager as a god-object behind a narrow
  trait risks the internals growing un-testable logic over time. → Mitigation:
  accept deliberately for this change; flag a follow-on
  `recording-manager-decomposition` change. The trait surface is the contract
  that keeps the stop path testable regardless of internal growth.
- **[Scope creep into the pipeline]** The port could tempt future changes to
  push mixing/VAD/saver logic across the boundary. → Mitigation: the trait is
  named `AudioCapturePort` and documented as lifecycle-only; pipeline
  internals stay in the adapter.

## Open Questions

- **Chunk receiver type for the trait signature.** Production uses
  `mpsc::UnboundedReceiver<AudioChunk>`. Exposing that exact type on the trait
  couples the port to tokio channels; exposing a custom `ChunkStream` trait
  adds abstraction. Lean toward the concrete `UnboundedReceiver` (KISS, YAGNI)
  unless the fake needs otherwise — resolve at task 2.1.
- **Whether `start` belongs on the port at all for this change.** The stop-path
  tests only need `stop_streams_and_flush`. But the fake needs to hand the use
  case a receiver to assert "no chunks after stop," which implies `start`
  produced one. Lean toward including `start` so the fake owns the full
  lifecycle — resolve at task 1.1.
