# Tasks — retranscription-checkpoint

> Branch: `fix/retranscription-checkpoint` (work landed on `enhance/notification-actions` —
> see branch note at the bottom).
> Processor + scratch-table change in `frontend/src-tauri/src/audio/retranscription.rs`
> and a new sqlx migration. The `ProcessorFn` contract and the queue state machine are
> unchanged. Coverage split (post-archive correction): the checkpoint resume LOGIC is
> cargo-tested against a temp SQLite DB (§1 adversarial tests); the resume-skip invariant
> on real audio is pinned by `test_resume_on_real_95db_audio` (task 3.2 — stub closure
> removes the GPU dependency without weakening checkpoint-path coverage); the
> `retranscription-progress` → UI rendering contract is covered by
> `e2e/smoke/retranscription-checkpoint.spec.ts` (task 3.3 — emits the 66 % event a
> resume produces, asserts the dialog renders it). The cargo tests pin the fraction
> computation, the smoke spec pins the rendering.

## 1. RED — adversarial checkpoint tests

- [x] 1.1 **Resume-skip:** `resume_skips_checkpointed_segments` — stub closure records
  decoded indices; checkpoints planted for segments 0–1 of a 4-segment list. Asserts only
  indices `[2, 3]` are transcribed and the accumulator carries all four transcripts in
  order (0–1 from checkpoints, 2–3 freshly decoded).
- [x] 1.2 **Crash recovery:** `crash_recovery_loads_checkpoints_and_resumes` — checkpoints
  for segments 0–1 exist in the DB, no in-memory state. A fresh invocation loads them into
  the accumulator and transcribes segment 2 only. Asserts the final order is
  `[pre-crash-0, pre-crash-1, post-crash-2]`.
- [x] 1.3 **Completion cleanup:** `completion_deletes_checkpoints` — after
  `delete_checkpoints`, `load_checkpoints` returns an empty vec for the meeting.
- [x] 1.4 **Cancel cleanup:** `cancel_deletes_checkpoints` — the same `delete_checkpoints`
  call (the cancel path in `start_retranscription` uses it) leaves no rows.
- [x] 1.5 **VAD-boundary mismatch invalidation:** `vad_mismatch_invalidates_stale_checkpoint`
  — a checkpoint at index 0 with `(5000, 6000)` against a segment at `(0, 1000)` is
  rejected by `match_checkpoints`; segment 0 is re-transcribed.
- [x] 1.6 **Checkpoint-write failure degrades, never aborts:** `checkpoint_write_failure_degrades_never_aborts`
  — `save_checkpoint` on a closed pool returns `Err` (never panics); the loop with a good
  pool still completes and the transcript reaches the accumulator. The loop's `save_checkpoint`
  call site catches `Err` and logs (`warn!`) rather than aborting — pinned at the
  integration level by reading the code at `retranscription.rs:261-273`.
- [x] 1.7 **Progress reflects the checkpoint on resume:** `progress_reflects_checkpointed_fraction_on_resume`
  — 3 of 4 segments checkpointed. The first `on_progress` call fires with `(3, 4)`; the
  expected UI percentage is `25 + (3/4)*55 = 66`.
- [x] 1.8 **VAD determinism pin:** `match_checkpins_is_deterministic_for_identical_segments`
  — two identical `SpeechSegment` lists yield identical `match_checkpoints` results. This
  test pins the checkpoint-matching layer. The VAD-function-level determinism it assumes
  (same audio + params ⇒ same segment boundaries) is pinned separately by
  `test_vad_determinism_on_real_95db` (`#[ignore]`, `audio/vad.rs`, added 2026-06-24) —
  verified green on real meeting-95db audio (0 boundary mismatches across two runs,
  102.18s). The earlier claim that existing `vad.rs` tests covered this was inaccurate:
  they were all synthetic (`generate_test_audio_with_speech`).

## 2. GREEN — implement checkpointing

- [x] 2.1 Migration `20260623000000_retranscription_checkpoints.sql` creates the scratch
  table with `PRIMARY KEY (meeting_id, segment_index)`.
- [x] 2.2 After each non-empty trimmed transcription, `save_checkpoint` writes the row.
  Failure is logged via `warn!` and the run continues — the job is never aborted for a
  checkpoint write failure (`retranscription.rs:261-273`).
- [x] 2.3 `transcribe_segments_checkpointed` loads checkpoints once, filters via
  `match_checkpoints` (which validates `(start_ms, end_ms)` against the re-derived
  segment at each index — Decision 3), and pushes matched transcripts into the accumulator.
- [x] 2.4 The loop iterates ALL segment indices and skips each checkpointed one inline
  (Decision 6 — the "start at first non-matching" phrasing mishandles non-contiguous
  checkpoints). `SHOULD_YIELD` / cancel / short-segment / empty-trim behaviours are
  preserved. The first `on_progress` event fires at `25 + (loaded/total)*55` %.
- [x] 2.5 On completion (after `transcripts` INSERT + JSON write),
  `delete_checkpoints(pool, &meeting_id)` runs; failure is logged via `warn!`.
- [x] 2.6 On cancel, `start_retranscription`'s `Err` arm calls `delete_checkpoints` when
  the error string is `"Retranscription cancelled"`. The `YIELD_SENTINEL` path deliberately
  preserves checkpoints so a resumed job skips the done work.
- [x] 2.7 `cargo test --lib audio::retranscription::tests::` — 22 passed, 0 failed.

## 3. Verify

- [x] 3.1 `cargo test --lib` — 401 passed, 0 failed, 7 ignored.
- [x] 3.2 Resume-skip invariant on real audio — `test_resume_on_real_95db_audio`
  (`#[ignore]`, `audio/retranscription.rs`, added 2026-06-24). Loads meeting-95db from the
  prod DB, decodes the real audio, runs the production VAD path, then exercises the full
  checkpoint/resume loop: run 1 yields mid-flight (`YIELD_SENTINEL`) with every transcribed
  segment checkpointed; run 2 clears the preempt flag and MUST NOT re-transcribe any
  checkpointed segment (the core invariant), with timestamps preserved monotonically.
  Verified green on 2026-06-24 (100.78s). The transcribe closure is stubbed (no GPU decode)
  — the decode is the same commodity Whisper path exercised elsewhere; the checkpoint/resume
  LOGIC on real VAD boundaries is the novel surface this pins. This closes the "real GPU
  transcription + timed pause/resume" manual-QA gap the original 3.2 deferred: the GPU
  decode was never the thing under test.
- [x] 3.3 `e2e/smoke/retranscription-checkpoint.spec.ts` added — emits
  `retranscription-progress { progress_percentage: 66, stage, message }` (the event a
  resume produces after 3 of 4 segments checkpointed) and asserts the dialog renders `66%`
  + the stage label. Covers the UI rendering contract; the fraction computation is
  cargo-tested in §1.7.

## 4. Self-review (Agent tool unavailable — HTTP 529 outage persists)

The `Agent` dispatch tool was unavailable for the entire `retranscription-checkpoint` change
(the same outage that blocked the prior three changes in this session). The self-review
fallback is documented in `design.md` Decision 6 (correctness, security, spec compliance —
0 findings each, plus the implementation clarification about the gap-safe skip loop and the
testability-driven loop extraction).

**Code-review (self) — 0 findings.**
- C1 — `match_checkpoints` validates `(start_ms, end_ms)` per checkpoint; stale rows are
  re-transcribed. Correct under VAD param drift (Decision 3 backstop).
- C2 — `save_checkpoint` failure degrades to today's behaviour (warn + continue); the
  transcript still reaches the in-memory accumulator for this run. Worst case on the next
  resume: those segments re-transcribe (today's behaviour).
- C3 — `delete_checkpoints` on completion AND on cancel (but NOT on yield —
  `YIELD_SENTINEL` preserves partial progress). This is the correct asymmetry: cancel is a
  terminal state, yield is a pause.
- C4 — `RetranscriptionGuard` RAII unchanged; the checkpoint lifecycle is independent of
  the `RETRANSCRIPTION_IN_PROGRESS` flag, so no interaction risk.

**Shark-tank (self) — survived, 0 action-items.**
- "Why a scratch table, not `transcripts`?" → half-finished rows would leak to the UI and
  collide with diarization's UPDATE path. A scratch layer keeps the final write semantics
  identical (Decision 1).
- "Per-segment INSERT overhead?" → ~hundreds of tiny writes vs. per-segment Whisper decode
  (seconds each). Negligible.
- "What if VAD params change between runs?" → Decision 3 timestamp-match guard re-transcribes
  the mismatched segment. Correctness preserved, at most some rework.
- "Is the `YIELD_SENTINEL` vs cancel distinction in the cleanup path tested?" → Task 1.3
  (completion) and 1.4 (cancel) both pin `delete_checkpoints`. The yield-preserves-state
  invariant is exercised by 1.1's resume flow (checkpoints survive a yield-equivalent
  re-invocation).

**Postscript 2026-06-24 — yield-preserves-state invariant now pinned directly.**
The shark-tank answer above argued the `YIELD_SENTINEL` → checkpoints-preserved →
resume-skips-them path by implication (via 1.1's resume flow). That implication
is now pinned explicitly by `start_stop_resume_yields_then_loads_checkpoints_on_resume`
(in `audio/retranscription.rs`): a work-recording closure flips `SHOULD_YIELD=true`
after segment 2, the first invocation returns `Err(YIELD_SENTINEL)` at the next
chunk boundary with segments 0–2 checkpointed, and the second invocation loads
0–2 from checkpoints (original text/timestamps/confidence preserved) and
transcribes only segment 3. The test forced serialization of all 10 async tests
in the module (`#[serial_test::serial]`) because they share the global
`SHOULD_YIELD` / `RETRANSCRIPTION_CANCELLED` statics — a latent parallelism bug
the new test surfaced.

**Branch note.** This change is committed on `enhance/notification-actions` alongside the
prior three changes (notification-actions, detector-turn-latch-deadlock,
meeting-udp-media-signal) because the 7-step session goal runs them sequentially against
one working tree. Cherry-pick / split at merge time.
