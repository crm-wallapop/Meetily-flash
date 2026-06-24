## Context

The current `stop_recording` Tauri command (`recording_commands.rs`, lines
475-889) executes its entire shutdown chain inline:

1. Stop CPAL streams + force-flush incremental saver (~1 s)
2. Wait for ALL queued transcription chunks — `tokio::time::timeout(600s, ...)`
3. Unload Whisper model
4. Emit analytics
5. Save audio file
6. `IS_RECORDING.store(false)` ← **only here**
7. Emit `recording-stopped`

The frontend's `RecordingStateContext` derives `isRecording` from the
`recording-stopped` event (and from polling `get_recording_state`, which also
reads `IS_RECORDING`). With 10 queued transcription chunks, step 2 took
**~2 minutes** in smoke tests on 2026-05-13 — leaving the status bar showing
"Recording" while the streams were dead.

Hexagonal boundaries (per CLAUDE.md §2a):
- `IS_RECORDING` is part of the **adapter** layer (Tauri command state).
- The shutdown sequence orchestrates multiple adapters (audio capture,
  transcription, file saver) — it belongs in a **use case**, not in a Tauri
  command.
- The frontend `RecordingStatusBar` is **UI**; it consumes events from the
  adapter via `RecordingStateContext`.

We are refactoring the shutdown sequence opportunistically as part of this
fix — moving it toward the target hexagonal structure rather than expanding
the inline command body further.

## Goals / Non-Goals

**Goals:**
- `RecordingStatusBar` clears (or transitions to "Saving…") within 1 s of the
  user pressing Stop, regardless of transcription backlog size.
- Audio capture halts within 1 s of the stop command (already true; preserve
  this).
- The user receives an unambiguous visual signal about whether stop was
  registered. No more split state between disabled button and "still recording"
  status.
- A separate "Saving…" state covers the background shutdown window. This state
  must be visually distinct from "Recording" (different label, no pulsing
  recording indicator).
- `stop_recording` Tauri command returns within ~1 s (i.e., as soon as streams
  are released and the background task is spawned).
- Idempotency preserved: a second `stop_recording` during the background
  window is a no-op.

**Non-Goals:**
- Cancelling the background shutdown (transcription drain, model unload, file
  save) — once started, it runs to completion. The user just doesn't see it
  blocking the UI.
- Speeding up transcription. The 2-minute drain is real work; we just stop
  blocking the UI on it.
- Reworking auto-detect or `meeting-ended` debounce timing. Those are tracked
  separately in `TODO.md`.
- Full hexagonal refactor of the audio adapter layer. We make this corner
  cleaner; we don't rewrite the audio module.

## Decisions

### D1: Three-state lifecycle replaces the boolean `IS_RECORDING`

Today: `IS_RECORDING` is `bool` (`true` while recording, `false` otherwise).
After: introduce a `RecordingPhase` enum stored as `AtomicU8` (Rust) with
three variants:

- `Idle` — no streams, no background work
- `Recording` — streams active, audio captured
- `Saving` — streams released, background shutdown in progress

The Tauri `recording-state-changed` event (new) emits the current phase. The
frontend `RecordingStateContext` exposes both `isRecording: boolean` (true
when `Recording`) and `isSaving: boolean` (true when `Saving`).

**Alternative considered:** keep `IS_RECORDING` bool and add a separate
`IS_SAVING` bool. Rejected — two flags can drift out of sync (both true, both
false) and require correlated updates. A single atomic enum is one source of
truth.

### D2: Background shutdown via `tokio::spawn`, not a long-lived task

The shutdown work (steps 2-5 of the current sequence) is wrapped in a single
async block and spawned via `tokio::spawn` from `stop_recording`. The handle
is stored in a `RwLock<Option<JoinHandle<()>>>` so subsequent `stop_recording`
calls can detect "shutdown already running" and return early.

**Alternative considered:** dedicated long-lived shutdown actor task that
receives messages over a channel. Rejected — YAGNI. We have one shutdown
per recording; a one-shot spawned task is simpler and disposable.

### D3: Stream release is the synchronous boundary; everything else is async

`stop_recording` does this synchronously before returning:
1. Acquire phase lock; if `Idle` or `Saving`, return Ok (idempotent).
2. Call `manager.stop_streams_and_force_flush().await` — this releases CPAL
   streams (~50 ms in practice) and force-flushes the incremental saver's
   in-memory buffer.
3. Set phase to `Saving`; emit `recording-state-changed { phase: Saving }`.
4. Spawn background task with the remaining work.
5. Return Ok.

If step 2 fails (audio adapter error), the phase still transitions to `Saving`
(streams may be in an indeterminate state but the user's intent is "stop") and
the background task handles cleanup-or-log. Better to be lenient on the user
side than to leave the UI in `Recording` forever.

**Alternative considered:** make the entire `stop_recording` fire-and-forget
(spawn before stream release too). Rejected — we need to know stream release
succeeded before flipping the phase, otherwise the audio file may grow after
the UI says "saved".

### D4: Backend shutdown errors surface as a passive event, not a blocking error

If the background task fails (Whisper unload fails, file save fails), it emits
a `recording-save-failed { error: string }` event. The frontend shows a toast
but the phase still transitions to `Idle` (we don't get stuck in `Saving`).
The recording's partial state is left for the existing startup GC pass.

**Alternative considered:** auto-retry. Rejected — YAGNI. We have no evidence
of intermittent shutdown failures. A toast + GC reconciliation is enough.

### D5: Frontend renders the new state with the existing `RecordingStatusBar`

`RecordingStatusBar` gets a discriminated render branch:
- phase `Recording` → red dot + "Recording" + gain readout + Stop button
- phase `Saving` → gray spinner + "Saving…" + no Stop button
- phase `Idle` → component returns `null`

The existing `isStopping` local guard in `RecordingControls.tsx` is removed
because the phase transitions now serve the same role (Stop button is
inherently hidden in `Saving` phase).

### D6: Atomic ordering — `SeqCst` everywhere

The phase atomic uses `Ordering::SeqCst` for both reads and writes. We are
not in a hot path (one transition per recording start/stop) and the cost of
a misordered phase observation (UI desync, double-shutdown) is high.

## Risks / Trade-offs

- **[Risk] Background task survives app shutdown of the recording session
  without saving.** → Mitigation: spawned task holds its work to completion
  unless the process exits. If the user quits Meetily during `Saving`, the
  audio is already on disk via incremental saver flush (step 2 of synchronous
  path); only the model unload and analytics emission would be lost, neither
  user-visible. Existing startup GC handles any orphan rows.

- **[Risk] User starts a new recording while in `Saving` phase.** → Mitigation:
  `start_recording` checks the phase and rejects with "another recording is
  finalizing — please wait" if not `Idle`. The `Saving` window is now ~seconds
  (no UI block) so this is much less painful than today's 2-minute block.
  Document the small remaining wait clearly.

- **[Risk] Audio file truncation if stream release returns before saver flush
  completes.** → Mitigation: `stop_streams_and_force_flush` is named for
  exactly this — it awaits the saver flush before returning. The contract is
  unchanged; we just stop waiting on transcription afterward.

- **[Risk] Phase desync between Rust atomic and emitted event.** → Mitigation:
  event is emitted immediately after the atomic store, both inside the same
  function, single task. The polling fallback (`get_recording_state` reads the
  atomic) keeps the UI eventually consistent even if an event is missed.

- **[Trade-off] Removing the `isStopping` frontend guard means the Stop button
  hides instead of going disabled-and-greyed.** → Acceptable. The transition
  is instant (atomic + event), and a hidden button cannot be double-clicked.

## Migration Plan

This is an internal refactor with no external API or DB schema change. No
migration steps for users or operators. Rollback is `git revert` of the
implementing commits.

## Open Questions

- **Naming**: `RecordingPhase` vs `RecordingState` (the latter is taken by
  `RecordingStateContext` in TS — disambiguate). Going with `RecordingPhase`
  for the Rust enum, keeping `RecordingState` for the TS context name.
- **Event channel**: stick with `recording-stopped` for backwards-compat
  (fires on transition to `Idle`) and add a new `recording-state-changed` for
  every phase transition. Or unify on a single event. Decision pending review
  of frontend listeners.
