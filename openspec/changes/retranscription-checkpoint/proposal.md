## Why

Pausing an in-flight retranscription and resuming restarts it from the beginning —
the progress bar climbs back to 5 % (decode), VAD re-runs, and every already-transcribed
segment is transcribed again. Root cause: retranscription accumulates transcripts in a
local `Vec` (`retranscription.rs:370`) and writes them to the DB only after the full
segment loop finishes (`:477`); the pause-yield path (`:378`) returns immediately,
discarding the in-memory work. For a long meeting, each pause/resume cycle wastes all
prior GPU transcription time, and repeated pauses compound the loss. The same discard
happens after a crash — the spec even codifies it ("partial transcripts from the crashed
session are discarded").

## What Changes

- **Per-segment checkpointing.** Each transcribed segment is persisted to a new scratch
  `retranscription_checkpoints` table `(meeting_id, segment_index, text, start_ms, end_ms,
  confidence)` immediately after it is transcribed, before the next segment begins.
- **Resume skips checkpointed segments.** On any (re)invocation — fresh start, resume after
  pause, or recovery after crash — the processor loads existing checkpoints, pre-populates
  the in-memory accumulator, and resumes the transcription loop at the first
  non-checkpointed segment. Decode + VAD still re-run (cheap relative to transcription);
  only the GPU-expensive transcription is checkpointed.
- **Progress reflects the checkpoint.** On resume the first transcription-progress emit
  reports the checkpointed fraction (not 5 %), so the user sees continuity.
- **Cleanup on completion and cancel.** When transcription finishes, the existing final-
  transcripts write proceeds, then the meeting's checkpoints are deleted. Cancel deletes
  them too.
- **Crash recovery now resumes.** Because checkpoints live in the DB, a job re-enqueued
  after a crash resumes from the last checkpoint instead of re-running from the start.
  The "crashed session" spec scenario is updated accordingly.

## Capabilities

### New Capabilities
<!-- None -->

### Modified Capabilities
- `post-meeting-pipeline`: ADD a requirement that retranscription checkpoints preserve
  partial progress across pause/resume and crashes (per-segment scratch persistence,
  resume-skip, cleanup on completion/cancel, VAD-mismatch invalidation, best-effort
  degradation); MODIFY the "Queue state persists across app restarts" crash-recovery
  scenario so a re-enqueued in-progress job resumes from the last checkpoint rather than
  discarding partial transcripts.

## Impact

- **Code**: `frontend/src-tauri/src/audio/retranscription.rs` (per-segment checkpoint
  INSERT; resume-skip load + accumulator pre-population; progress-from-checkpoint;
  completion/cancel cleanup); a new sqlx migration creating `retranscription_checkpoints`;
  repo methods alongside the existing transcript INSERT. The `ProcessorFn` contract and
  the queue state machine are unchanged.
- **Spec**: `openspec/specs/post-meeting-pipeline/spec.md` — new checkpoint requirement +
  edited crash-recovery scenario.
- **Tests**: adversarial — pause mid-run then resume skips checkpointed segments (no
  re-transcribe); crash simulation (re-invoke with no in-memory state) resumes from
  checkpoint; completion deletes checkpoints; cancel deletes checkpoints; VAD-boundary
  mismatch invalidates a stale checkpoint; checkpoint-write failure degrades to today's
  restart behavior without aborting the job.
- **User-visible**: pause/resume preserves progress (no restart to 5 %); crash recovery
  preserves progress. No UI change beyond accurate progress.
- **Risk**: low–medium. Checkpoints are a scratch layer deleted on completion; worst case
  (checkpoint write fails, or VAD regenerates different boundaries) degrades to today's
  restart-from-scratch behavior. VAD determinism (same audio + params ⇒ same segment
  boundaries) is the key invariant, pinned by a test and defended by the mismatch check.
