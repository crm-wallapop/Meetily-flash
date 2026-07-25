## Why

On multi-speaker meetings, a Whisper transcript segment that spans two speakers is shown under a single speaker label (verified on `meeting-cde5c264-…`: a 26.8 s window of two-speaker dialogue carries one label). The canonical `speaker-diarization` spec **already mandates** the fix: "the text SHALL be split at the speaker change boundary, producing separate transcript rows per speaker" and "the original transcript row is replaced by two rows" (`openspec/specs/speaker-diarization/spec.md:90,105`). The implementation does not conform.

Both write paths — `run_diarization_for_meeting` (`commands.rs:533-538`, the live production path) and `DiarizationProcessor::process` (`diarization_processor.rs:187-192`, currently dead — the transcription queue is wired with `None`) — persist the aligned per-speaker splits via `update_transcript_speaker(original_id, …)`, which is `UPDATE transcripts SET speaker_label=? WHERE id=?` (`speaker.rs:244`). All N aligned splits derived from one source row share that row's `id`, so the N updates hit the same row and **last-writer-wins**: one speaker label survives, the split text is never written. The split is computed by `align_transcripts_with_diarization` (`alignment.rs`, pure domain, tested) and then thrown away.

This is a conformance fix against an existing mandate, not a new behavior. The spec left the *persistence mechanics* implicit; this change makes them explicit (column inheritance, idempotency, the proportional-tail edge case) so the loophole that allowed compute-then-discard cannot recur.

> **Scope note (2026-07-24).** Fixing this defect yields a 2-way split of the evidence window on cde5c264 — necessary but, on its own, not per-turn resolution of rapid back-and-forth. The diarization's own boundary granularity (uniform chunk grid) is a separate ceiling addressed by the companion change `diarization-pyannote-boundaries`, which sources finer boundaries from pyannote's `OfflineSpeakerDiarization`. This change must land first (Part B's finer boundaries produce more splits, which need this persist path).

## What Changes

Replace the per-`AlignedSegment` `UPDATE`-by-id loop in both write paths with a transactional split-and-persist: delete the source coarse row, insert N per-speaker rows (or in-place `UPDATE` when N=1). New rows carry fresh UUIDs and copy every source column except the overrides: split `transcript` text, clamped `audio_start_time`/`audio_end_time`, `speaker_label`, `speaker_source='auto'`, recomputed `duration`, and **NULL `token_timestamps`** (see design D1/D5 for why NULL is load-bearing for re-diarization idempotency). A source row with `speaker_source='manual'` is left untouched.

Fix the `align_proportional` tail (`alignment.rs:228-243`): when no diarization segment overlaps a source row's range, the row is labeled "Unknown Speaker" with its own timing, instead of borrowing a foreign speaker and emitting `audio_start_time > audio_end_time`.

Declare and enforce the invariant that diarization for a given `meeting_id` is mutually exclusive across the two paths (a meeting-level guard serializes them).

**Out of scope:** finer speaker boundaries from pyannote (companion change `diarization-pyannote-boundaries`); deleting the dead `DiarizationProcessor::process` path (fixed defensively here; removal is separate cleanup); re-transcription to populate `token_timestamps` (the persist path works without them).

## Capabilities

### New Capabilities
(none)

### Modified Capabilities
- `speaker-diarization`: amend the "Token-level timestamps align…" requirement to make the persistence mechanics explicit — delete-source + insert-N in one transaction; column inheritance (NULL `token_timestamps`, recomputed `duration`, fresh UUID, `speaker_source='auto'` on N=1); re-diarization idempotency; the proportional-tail "Unknown Speaker" fallback; mutual-exclusion guard. The outcome ("replaced by two rows") is already mandated; this delta specifies *how* it is persisted.
- `post-meeting-pipeline`: amend the "Diarizing processor" requirement's step 7 ("Update transcript rows with `speaker` labels and `speaker_source = "auto"`") to reference the split-and-persist operation, so the two specs do not contradict.

## Impact

- **Code**: `audio/speaker/commands.rs` (`run_diarization_for_meeting` storage loop, ~531-539), `use_cases/diarization_processor.rs` (`process` storage loop, ~186-193 — dead in production, fixed defensively), `database/repositories/speaker.rs` (new transactional `persist_aligned_splits`; N=1 stays an in-place `UPDATE` of both `speaker_label` and `speaker_source`), `audio/speaker/alignment.rs` (`align_proportional` tail fix at ~228-243).
- **Data shape**: a coarse Whisper row may become N rows after diarization. Transcript `id` is not FK-referenced by any table (verified across all 21 migrations). Summary is text-based, keyed by `meeting_id`. Re-transcription does `DELETE FROM transcripts WHERE meeting_id=?` (`retranscription.rs:669`) before inserting fresh rows, so split rows are cleanly replaced.
- **User-visible tradeoff**: splitting a row that carries `token_timestamps` discards them on the split rows (set to NULL) until the meeting is re-transcribed. This is required for re-diarization idempotency (design D1/D5). For meetings recorded before the token-timestamps feature (e.g. cde5c264, 0/238 rows), there is no data loss.
- **Spec**: deltas against `speaker-diarization` and `post-meeting-pipeline`.
- **Tests**: §4 adversarial — real SQL-injection payload, re-diarize idempotency (strict equality + id-set + NULL-tokens, NOT non-decreasing), transaction atomicity under injected failure, copy-all-columns-verbatim, MIN_SPEECH_SECS floor, CJK/no-whitespace proportional split, oversized token blob, SQLite host-param limit, malformed `token_timestamps` JSON, prompt-injection-text-as-data, concurrent-distinct-sources, plus a property-based test with word-preservation / time-coverage / duration / ordering / non-empty / speaker-containment invariants across both token and proportional paths.
