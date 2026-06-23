## Context

Retranscription (`audio/retranscription.rs`) transcribes a meeting MP4 by: decoding →
VAD → (deterministic segment split at 25 s) → per-segment Whisper/Parakeet decode →
accumulate into a local `Vec<(text, start_ms, end_ms)>` → write the `transcripts` table
once after the loop → write JSON → diarize. The pause path sets `SHOULD_YIELD`; the
processor checks it before each segment and `return Err(YIELD_SENTINEL)` on a hit
(`:378`), dropping the accumulator. `resume_all` / `run_job_now` flips the job back to
`Pending`; the worker re-dispatches the processor, which runs from the top. Hence the
"back to 5 %" the user observed. Crash recovery re-enqueues the same way, with the same
restart.

The decode + VAD + split steps are deterministic for fixed params on the same decoded
audio, so the `processable_segments` list (and its indices + `(start_ms, end_ms)`) is
stable across invocations. That stability is what makes per-segment checkpointing safe.

## Goals / Non-Goals

**Goals:**
- Pause/resume continues from where it stopped — no re-transcription of done segments.
- Crash recovery resumes from the last persisted checkpoint.
- Progress after resume reflects the checkpointed fraction.
- Zero change to the final `transcripts` table content or the `ProcessorFn`/queue contract.

**Non-Goals:**
- No checkpointing of the decode or VAD stages (they re-run; cheap vs. transcription).
- No checkpointing of the summary or diarization phases (separate processors; out of
  scope — those are not the reported pain point and have different resumability shapes).
- No UI changes beyond accurate progress.
- No change to the in-memory `TranscriptionQueue` / IndexedDB mirror schema.

## Decisions

### Decision 1 — Scratch `retranscription_checkpoints` table (sqlx migration)
`CREATE TABLE retranscription_checkpoints (meeting_id TEXT, segment_index INTEGER,
text TEXT, start_ms REAL, end_ms REAL, confidence REAL, PRIMARY KEY (meeting_id,
segment_index))`. Written per-segment; deleted on completion and on cancel.

**Why a scratch table, not the `transcripts` table:** the final `transcripts` rows are
derived by `create_transcript_segments` from the full accumulator and are later mutated by
diarization. Writing partial rows there mid-flight would (a) expose half-finished
transcripts to the UI/DB before completion and (b) tangle with diarization's UPDATE path.
A scratch layer keeps the final table's write semantics identical whether the result came
from one run or a checkpointed run, and cleans up to leave no trace on success.

**Alternatives considered:**
- *Hold partial transcripts in the queue `Job` struct.* Requires widening `ProcessorFn`'s
  return to carry partial state, pollutes `QueueSnapshot` (mirrored to IndexedDB) with
  large arrays, and is lost on crash. Rejected.
- *Write partial rows directly to `transcripts`.* Exposes half-finished data and collides
  with diarization. Rejected.
- *Checkpoint the decoded samples / VAD output.* Large; re-decode is cheap relative to
  transcription. Rejected.

### Decision 2 — Resume loads checkpoints, pre-populates the accumulator, skips done segments
After VAD + split reproduces `processable_segments`, query
`SELECT … WHERE meeting_id = ? ORDER BY segment_index`. For each checkpoint whose
`(start_ms, end_ms)` matches the re-derived segment at that index, push its `(text,
start_ms, end_ms)` into `all_transcripts` and skip its transcription. Start the loop index
at the first non-matching segment; emit the first transcription-progress event at
`25 + (loaded_count / total) * 55` %.

The loop's existing `SHOULD_YIELD` check, short-segment skip, and empty-trim logic run
unchanged for the segments actually transcribed.

### Decision 3 — Key by `segment_index`, validate by `(start_ms, end_ms)` match
Checkpoints are keyed `(meeting_id, segment_index)` for a fast resume load. Because VAD is
deterministic the indices align in practice, but a checkpoint whose timestamps do not
match the re-derived segment at that index is treated as stale and re-transcribed
(Scenario: VAD boundary mismatch invalidates a checkpoint). This defends correctness if
VAD params ever change between runs or a checkpoint belongs to a different audio file
reusing a meeting_id (should not happen, but cheap to guard).

### Decision 4 — Cleanup on completion and cancel; best-effort on write failure
- **Completion:** after the existing `transcripts` INSERT + JSON write succeed,
  `DELETE FROM retranscription_checkpoints WHERE meeting_id = ?`.
- **Cancel:** the existing `cancel_retranscription()` path gains a checkpoint DELETE so a
  cancelled job leaves no scratch rows (a later re-transcription of the same meeting starts
  clean).
- **Write failure:** a failed checkpoint INSERT is logged and the segment's transcript is
  still kept in the in-memory accumulator for this run; on the next resume the un-
  checkpointed segments re-run. The job is never aborted solely because checkpointing
  failed — worst case degrades to today's restart behavior.

### Decision 5 — Crash-recovery scenario updated
The current "In-progress job from a crashed session is recoverable" scenario says the
worker "re-runs from the start of the MP4; partial transcripts … are discarded." With
checkpoints in the DB, recovery re-invokes the processor, which loads checkpoints and
resumes. The scenario is edited to say so (see delta spec). No change to the recovery
modal / IndexedDB flow — only the processor's behaviour on re-invoke changes.

## Risks / Trade-offs

- **[VAD non-determinism across versions/params]** → Decision 3 timestamp-match guard
  re-transcribes any mismatched segment; correctness preserved, at most some rework.
- **[Stale checkpoints for a re-transcribed meeting]** → A meeting re-transcribed after an
  earlier completed run has no checkpoints (deleted on completion). A cancelled-then-
  re-run meeting has its checkpoints deleted at cancel. The timestamp guard is the
  backstop for any residual staleness.
- **[Checkpoint write overhead]** → one small INSERT per segment (~hundreds per meeting);
  negligible vs. the per-segment Whisper decode (seconds). Acceptable.
- **[Partial transcripts visible mid-run]** → avoided by the scratch-table design; the
  `transcripts` table is still written once at completion.

No data migration beyond the new table. Rollback = drop the table + revert the processor.

## Decision 6 — Round-1 self-review (0 findings, one implementation clarification)

The `Agent` tool is not available in this session; the prior three changes were closed
under a persistent HTTP 529 outage. The same self-review fallback applies here.

**Correctness — 0 findings.**

- **C1 — Problem diagnosis verified.** `all_transcripts: Vec<(String, f64, f64)>` at
  `retranscription.rs:370`; the yield path at `:378` returns `Err(YIELD_SENTINEL)`
  discarding the accumulator; the final `transcripts` INSERT loop at `:477`. The
  claim "pause/resume restarts from the beginning" is accurate against the code.
- **C2 — VAD determinism is the load-bearing invariant, and it is defended in depth.**
  Decode + VAD + split are deterministic for fixed params on the same decoded audio
  (VAD uses `VAD_REDEMPTION_TIME_MS` and fixed thresholds). Decision 3's
  `(start_ms, end_ms)` timestamp-match guard is the backstop if VAD params ever drift
  between runs. Task 1.8 pins the invariant. The design correctly prefers rework over
  trusting a stale checkpoint.
- **C3 — Scratch table isolates the checkpoint lifecycle cleanly.** PRIMARY KEY
  `(meeting_id, segment_index)`; per-meeting isolation; deleted on completion and
  cancel. No risk of half-finished transcripts leaking to the UI (they never touch
  the `transcripts` table mid-flight).
- **C4 — Best-effort checkpoint writes are the right degradation mode.** A failed
  INSERT is logged and the segment's transcript still reaches the in-memory accumulator
  for the current run; the job is never failed solely for a checkpoint write failure.
  Worst case on the next resume: the un-checkpointed segments re-transcribe (today's
  behaviour for those segments). Task 1.6 pins this.

**Implementation clarification (not a finding) — the skip loop iterates ALL segments.**

Decision 2 says "Start the loop index at the first non-matching segment." Taken literally,
this mishandles the gap case: a short segment (`< 1600` samples) or an empty-transcription
segment produces NO checkpoint, so checkpoints can be non-contiguous (e.g. indices
{0, 2, 3} with a gap at 1). Starting the loop at "the first non-matching segment" (index 1)
and running forward would then RE-transcribe index 2 and 3 — wasting the checkpoint work.

The correct implementation iterates ALL segment indices and skips each checkpointed one
inline (push loaded transcript, `continue`). Non-checkpointed segments (gaps, short,
empty) are re-processed naturally. This is O(checkpointed_count) of cheap HashMap lookups
plus O(non_checkpointed_count) of GPU transcription — the same total transcription work as
the idealised "jump to first non-matching" but without the gap bug. The SHOULD_YIELD /
cancel checks run per iteration as today; checkpointed iterations are O(1) so the overhead
of re-checking them after a yield is negligible. The progress formula
`25 + (i / total) * 55` naturally reflects the loaded fraction because `i` advances through
checkpointed segments; the first emitted progress for a non-checkpointed segment reports
the right percentage.

**Testability — the segment loop is extracted behind a transcription closure.**

`run_retranscription` is a ~350-line monolith tightly coupled to `AppHandle`, the
Whisper/Parakeet engines, the file system, and Tauri events. It is not unit-testable as-is.
The adversarial tests (1.1, 1.2, 1.7) require driving the checkpoint-aware loop with a
stub transcription closure and a temp SQLite DB. The implementation therefore extracts the
loop into a function that takes:
  - `meeting_id: &str`
  - `processable_segments: &[SpeechSegment]`
  - `pool: &sqlx::SqlitePool`
  - a `transcribe: impl Fn(usize, &SpeechSegment) -> Future<Output=Result<(String, f32)>>` closure
  - an `on_progress: impl Fn(usize, usize)` callback (decoupled from `AppHandle` so the loop
    is testable without Tauri)

The checkpoint DB helpers (`save_checkpoint`, `load_checkpoints`, `delete_checkpoints`,
`match_checkpoints`) are module-level functions taking `&sqlx::SqlitePool`, independently
testable against a temp DB. This is the minimum extraction required by adversarial TDD —
not speculative abstraction.

**Security — 0 findings.**

- **S1 — Scratch table is internal-only.** No untrusted input reaches the checkpoint
  schema fields: `meeting_id` is an internally-generated UUID; `segment_index` is a
  loop counter; `text` is Whisper output (already trusted internally per CLAUDE.md §1.9);
  `start_ms`/`end_ms`/`confidence` are float scalars from VAD/Whisper. No SQL injection
  surface (parameterised queries via sqlx `.bind()`).
- **S2 — No new OWASP-relevant surface.** The checkpoint table is never exposed via an
  API endpoint, Tauri command, or frontend read path. It is write-only from the processor
  and read-only from the processor's resume path.

**Spec compliance — 0 findings.**

- **SC1 — ADDED requirement ("Retranscription checkpoints preserve partial progress")**
  carries 6 scenarios that map 1:1 to tasks 1.1-1.6. Each scenario names a behaviour the
  implementation must exhibit; each task names the test that pins it.
- **SC2 — MODIFIED "Queue state persists across app restarts" scenario** correctly
  narrows the crash-recovery path from "partial transcripts are discarded" to "resumes
  from the last persisted checkpoint." The implementation note about the in-memory queue
  + IndexedDB mirror is preserved verbatim.
- **SC3 — No scope creep.** Summary and diarization phases are explicitly Non-Goals; the
  `ProcessorFn` contract and the queue state machine are unchanged.

**Conclusion.** Proceed to `/opsx:apply` with the loop-extraction approach for testability.
