## ⚠️ DEFERRED & architecture corrections (2026-06-24)

**Status: DEFERRED.** Parked; do not implement from the original Context or
D1–D6 below without reading this section — **they were written against
incorrect premises about the current code.**

### Re-examined 2026-06-25 — decision stands, two reasoning errors corrected

A re-exploration confirmed the deferral still holds (no new caller; YAGNI), but
two of the deferral's own claims below are wrong and would mislead the next
person who opens this change.

**Correction 1 — "Option 1 is tautological" overstates it.** `background_shutdown`
(`recording_commands.rs:702-827`) is not "call flush and done" — it is an
error-path state machine: flush with continue-on-error (the comment at `:715`
cites design D3), a 300 s `save_recording_only` timeout (`:762`) whose
ERR/TIMEOUT branches clear the gate and return (`:770-778`), an idempotent
SQLite transcript save (`:802` `MeetingAlreadyExists`), and a data-extraction
step before the DB write (`:787-791`). A fake `AudioCapturePort` + an extracted
shutdown use case would test that sequencing — not nothing. The "tautological"
label is too strong.

**Correction 2 — the value frame ignored the shutdown error paths.** "Why
deferred" and "What is already covered" measure value as G1 + G2 only. But the
two historical bugs this path is notorious for — the 2-minute stop lag and
`folder_path = null` — lived in exactly those shutdown error paths (flush/save
timing, data extraction), which are untested and which a manager port *would*
reach. So "what's at stake" was mis-stated: it is not two already-covered
guarantees, it is the bug-adjacent error surface, none of it cargo-tested.

**Cost correction — Option 1.5 exists and is much cheaper than Option 2.** The
16-site `RECORDING_MANAGER` global→app-state migration ("The Option 2 path"
step 1) is only required for full D3 — command-level DI — and the command level
is already phase-machine-tested (G1). `background_shutdown` already receives the
owned `Option<RecordingManager>` from `.take()`, so changing that one signature
to `Option<Box<dyn AudioCapturePort + Send>>` and boxing the real manager at
the spawn site makes the shutdown use case fake-injectable **without touching
the 16 sites.** Call this Option 1.5: ≈ 1 trait + impl + 1 signature refactor +
extracted use case + fakes.

**Honest bound on Option 1.5's value.** `background_shutdown` is also coupled
to `AppHandle` (`app.emit`, `app.state::<TranscriptionQueueState>`,
`app.state::<AppState>`) and the `RECORDING_PHASE` global. So a manager port
reaches the flush + save + extraction *sequencing* — but NOT the richest logic
in the function: the M1/M2 `compare_exchange(Saving→Idle)` phase race (`:743`)
and the DB-save branches. Those stay untested until `AppHandle`/state is also
abstracted, which drifts back toward Option 2's cost. The slice is real but
bounded.

**Why YAGNI still holds despite the corrections.** The reframe shows the value
was *undercounted*, not that it *clears the bar*. The detection-port precedent
does not transfer cleanly: `step_detector` has dense state-machine branching
(TURN/UDP, stable/flaky, entry/exit) — which is why the §4 detector coverage
committed 2026-06-25 (`device_disconnect_mid_call…`, `rapid_leave_rejoin…`,
`turn_latch…` in commit f067e6e) was only writable because
`MeetingDetectorPort` existed. The recording shutdown, by contrast, is mostly
straight-line error handling with the one interesting race (M1/M2) sitting
behind `AppHandle` coupling that Option 1.5 cannot cross. No new caller has
appeared. "Latent risk on bug-adjacent untested error paths, nothing biting"
is the speculative-build case YAGNI exists to stop.

### What the code actually does (the original design got these wrong)

1. **The flush is NOT on the synchronous stop path.** `stop_streams_and_force_flush`
   runs in `background_shutdown` (`recording_commands.rs:714`), inside the
   `tokio::spawn`'d task — it was *deliberately moved out of the sync path* (the
   comment at `:708-711` says "moved here from stop_recording's synchronous path so
   the command returns fast"). The sync path (`stop_recording :514-696`) only does:
   phase guard → CAS `Recording→Saving` → `take()` the manager → extract id/folder
   → emit Saving + recording-stopped → spawn. It never touches streams.
2. **`RecordingManager` is a process-global static, not Tauri app state.** It is
   `RECORDING_MANAGER: Mutex<Option<RecordingManager>>`, and `stop_recording`
   **consumes** it via `.take()` (`:562`) — it does not borrow it. So D3
   ("`Arc<dyn AudioCapturePort>` in app state") and tasks §5 are impossible without
   first migrating the manager from global-static to app-state — a refactor the
   original design never scoped.
3. **There is no flush call on the sync path to extract.** Tasks §2.2 ("lift the
   synchronous stop body to inject a port flush") has no injection point.

### What is already covered without this change

- **G1 (status bar clears <1s):** already a permanent `cargo test` gate — the sync
  path's CAS never touches streams, and the phase-machine tests
  (`stop_sync_path_transitions_phase_to_saving_and_returns_fast` et al.) assert the
  fast flip.
- **G2 (capture halts / no chunks after stop):** covered by the `#[ignore]`
  real-device test `real_device_stop_releases_streams_within_1s_and_halts_capture`
  (merged 2026-06-24) — asserts `stop_streams_and_force_flush` returns <1s and
  `active_stream_count()==0`. Runs via `cargo test -- --ignored` with a mic.

### Why deferred

- **Option 1 (minimal port at the flush seam):** rejected — its §3 cargo tests are
  largely tautological (the no-chunks-after-stop fake test asserts the fake stops
  sending — true by construction) or redundant with the existing G1 phase-machine
  gate. It would prove the use case *calls* `port.stop`, little more.
- **Option 2 (full DI):** correct architecture but disproportionate now — requires
  migrating `RECORDING_MANAGER` global-static → app-state across every access site
  on the critical recording path (where the 2-min-lag and `folder_path=null` bugs
  lived), for value (full §2a) with **no current caller** (no §4 change exists
  yet). That is the YAGNI case (CLAUDE.md §6).
- Net: G1 is already permanently gated, G2 is already `#[ignore]`-covered; both
  options are either thin (1) or speculative+risky (2).

### Trigger to revisit (sharpened 2026-06-25)

Either of:
1. A concrete **§4 adversarial need** that requires faking the capture
   lifecycle without real hardware — device-disconnect mid-recording,
   permission-denied, or sample-rate-mismatch; OR
2. **Any change that touches the `background_shutdown` error paths** (flush /
   `save_recording_only` / data extraction / analytics in
   `recording_commands.rs:702+`). In that case the port pays off *inside that
   same change* — the work becomes testable rather than speculative — and
   **Option 1.5** (port at the `background_shutdown` signature only, no
   app-state migration) is the right scope, not full Option 2.

Whichever fires first flips the recommendation to build.

### The Option 2 path (when revisited)

Corrected implementation against the real architecture:

1. **Migrate the manager to app state (prerequisite).** Convert
   `RECORDING_MANAGER: Mutex<Option<RecordingManager>>` →
   `Arc<RwLock<Option<RecordingManager>>>` in Tauri state via `app.manage()`.
   Update every access site: `start_recording` (`recording_commands.rs:175,413`),
   `stop_recording` (`:542`), cancel paths, `get_state`, the startup GC, tray, and
   the `get_recording_state` command. The `.take()` on stop becomes a
   `.write().take()` on the app-state lock.
2. **Define `AudioCapturePort`** (`ports/audio_capture.rs`):
   `async fn start(...) -> Result<mpsc::UnboundedReceiver<AudioChunk>>` +
   `async fn stop_streams_and_flush(&mut self) -> Result<()>`. `impl` it for
   `RecordingManager` (delegate to existing methods).
3. **Composition root (corrected D3).** In `lib.rs` construct ONE
   `Arc<RecordingManager>`; clone + upcast into
   `Arc<dyn AudioCapturePort + Send + Sync>`; register both in app state. Sole
   cross-boundary importer per §2a.
4. **Wire the port into `background_shutdown`.** Its signature changes from
   `manager: Option<RecordingManager>` → `manager: Option<Box<dyn AudioCapturePort>>`;
   the flush at `:714` then goes through the trait. The sync path's CAS is untouched
   (it is the G1 gate, already phase-machine-tested).
5. **Fake + §3 tests** (`use_cases/recording_lifecycle`). Now meaningful because the
   whole capture lifecycle — not just the flush — is behind the swappable port, so
   device-disconnect / permission-denied / sample-rate-mismatch can be simulated.
6. **Keep the `#[ignore]` real-device test** as the adapter confirmation (D5:
   port = permanent logic gate; `#[ignore]` = hardware confirmation).

---

> The original Context + Decisions below are preserved for history. They contain
> the factual errors corrected above.

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
