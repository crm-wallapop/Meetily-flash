## Context

A Whisper transcript segment that spans two speakers is persisted under a single label. The canonical `speaker-diarization` spec mandates the split ("replaced by two rows", `spec.md:90,105`); the implementation does not conform.

Both write paths persist aligned per-speaker splits via `update_transcript_speaker(original_id, …)` = `UPDATE transcripts SET speaker_label=? WHERE id=?` (`speaker.rs:244`):
- `run_diarization_for_meeting` (`commands.rs:533-538`) — the live production rediarize path (passes real transcript segments to the adapter at `commands.rs:392`).
- `DiarizationProcessor::process` (`diarization_processor.rs:187-192`) — currently dead (the transcription queue is wired with `None` at `transcription_queue.rs:314`; `DiarizationProcessor::new` is only constructed under `#[cfg(test)]`).

All N `AlignedSegment`s derived from one source row share that row's `original_id` (`alignment.rs:159/181/217`), so N updates hit the same row → last-writer-wins → one label survives, split text never written. The split is computed correctly by `align_transcripts_with_diarization` (token path `align_with_tokens:113`, proportional fallback `align_proportional:171`, both tested) and discarded.

Constraints (all verified against source): transcript `id` is not FK-referenced by any table (21 migrations, all FKs target `meetings(id)` or `speakers(id)`). Summary is text-based, keyed by `meeting_id`. Re-transcription does `DELETE FROM transcripts WHERE meeting_id=?` in a transaction (`retranscription.rs:665-669`). `clear_auto_speaker_labels` filters `WHERE speaker_source = 'auto'` (`speaker.rs:300`); `update_transcript_speaker` carries the `speaker_source != 'manual'` guard (`speaker.rs:244`).

## Goals / Non-Goals

**Goals:**
- Conform to the existing "replaced by two rows" mandate: persist aligned per-speaker splits as separate transcript rows in both write paths.
- Make the persistence mechanics explicit so compute-then-discard cannot recur: column inheritance, idempotency, the proportional-tail edge case, mutual exclusion.
- Preserve every prior invariant: temporal-coherence smoothing, label-quality `refine_pass2`, most-isolated-cluster cap, cross-meeting registry matching, single-speaker rows not split, all words preserved across splits.

**Non-Goals:**
- Finer speaker boundaries (companion change `diarization-pyannote-boundaries`).
- Deleting the dead `DiarizationProcessor::process` path (fixed defensively here).
- Re-transcription to populate `token_timestamps`.
- Schema migration (no new columns/tables).

## Decisions

**D1 — Split-in-place; copy-all-except-overrides; NULL `token_timestamps`; recompute `duration`.** The source coarse row is deleted and replaced by N per-speaker rows. New rows copy every source column and override only: fresh UUID `id`, split `transcript` text, clamped `audio_start_time`/`audio_end_time`, `speaker_label`, `speaker_source='auto'`. Two overrides are load-bearing: (a) `token_timestamps` set to NULL on split rows — `align_with_tokens` is not gated on the row's own time range (`alignment.rs:113-168`), so inheriting the source's full tokens would let a later re-diarize re-expand a fine row and undo the split (see D5); NULL forces the range-bounded proportional path. (b) `duration` recomputed from each split's clamped timing — inheriting the source's 26.8 s onto each 13 s row reaches the frontend and the flicker gate (`commands.rs:1427`, `mid[i].duration < 5.0`). "Copy every source column" is verified by a test that asserts column equality for every column except the overrides, so a future migration adding a column is caught.

*Alternatives:* child table `aligned_segments` (adds a table+join and duplicates rendering/override logic — rejected; the mandate's "replaced by two rows" wording and the FK analysis make split-in-place strictly simpler); JSON column (forces renderer/override to parse — rejected).

**D2 — Shared `persist_aligned_splits` normalizes persistence, not alignment.** Both write paths group `AlignedSegment`s by `original_id` and call one transactional repository routine (delete-source + insert-N for N>1; in-place `UPDATE` for N=1). The two paths pass *different* inputs to the adapter (live path: real transcript segments; dead path: `&[]`), so the shared routine unifies only persistence.

**D3 — N=1 is an in-place `UPDATE` of BOTH `speaker_label` AND `speaker_source='auto'`.** Single-speaker rows keep their `id`; no row is created or deleted. Setting `speaker_source='auto'` is required so the row remains visible to `clear_auto_speaker_labels` (`speaker.rs:300`) on the next re-diarize — otherwise stale labels survive.

**D4 — Skip rows with `speaker_source='manual'`.** The guard is the sole protection on the dead processor path (which does not pre-clear labels) and defense-in-depth on the live path.

**D5 — Re-diarization is monotonic refinement; re-transcription is clean-slate.** Once split, rows carry NULL tokens (D1), so re-diarize aligns each fine row within one speaker segment → N=1 → in-place relabel; the split is never re-expanded. Re-transcription does `DELETE FROM transcripts WHERE meeting_id=?` before inserting fresh coarse rows, so split rows are destroyed and replaced — safe by construction. A future "soft" re-transcription that updated text without deleting rows would violate this and must be treated as a contract breach.

## Risks / Trade-offs

- **[Token data loss on split]** Splitting a row that carries `token_timestamps` discards them (NULL) until re-transcription. Required for idempotency (D1/D5). Disclosed as a user-visible tradeoff in the spec. For pre-feature meetings (e.g. cde5c264, 0/238 rows) there is no loss.
- **[Drops words at the proportional tail]** The `align_proportional` tail (`alignment.rs:228-243`) is fixed: no-overlap falls back to "Unknown Speaker" with the source row's own timing, instead of borrowing `diarization.last()`'s speaker and emitting `audio_start_time > audio_end_time`. A property-based test covers arbitrary source/diarization layouts.
- **[Prompt-injection transcript text]** Bound via sqlx `?`; treated as opaque data; survives the split verbatim. A real SQL-meta-char payload (`'; DROP TABLE transcripts; --`) is a distinct §4 category from prompt injection and has its own test.
- **[Concurrent diarize paths on one meeting_id]** Declared invariant: diarization for a meeting is mutually exclusive (meeting-level guard); the two paths must not run simultaneously.
- **[SQLite host-param ceiling]** N=100 splits × ~12 columns approaches the 999-host-param default ceiling; chunked inserts if exceeded.
- **[Row identity churn confuses React keys]** Fresh UUIDs per split; transcript list keys by `id`. Smoke verifies.

## Migration Plan

No schema migration. Code-only: write paths + repository + alignment tail. Rollback = revert code. Existing meetings keep prior labels until re-diarized. The spec delta records the now-explicit persistence mechanics.

## Security Model

Transcript text is untrusted Whisper output (§9). `persist_aligned_splits` binds all text via sqlx `?` placeholders; split text is opaque end-to-end (injection and SQL-meta-char payloads survive verbatim — adversarial tests). No new untrusted input surface. Speaker names bound via `sanitize_speaker_name` unchanged.

## Adversarial Tests (§4)

Split-and-persist replaces source with N rows (non-regression — fails under any `UPDATE`-by-id scheme); N=1 in-place keeps id and sets `speaker_source='auto'`; **copy-all-columns-verified-dynamically** (`PRAGMA table_info` against an override-exclusion set, so a future migration column — or the currently-present `previous_label` from migration 20260609 — is caught, not silently dropped); word-preservation invariant (token + proportional); **real SQL-injection payload** (`'; DROP TABLE transcripts; --` — the verbatim-persistence half is the differentiator; sqlx rejects stacked statements regardless, so the "table survives" half alone would not catch interpolation); prompt-injection text as data; manual-source skip; **transaction atomicity** (a dedicated test pool with a real `CHECK (transcript <> '__FAIL__')` constraint induces a reproducible mid-write failure on the second split's text → rollback → source row survives; confirms the delete + N inserts run as one sqlx transaction. The original design proposed a fail-after-N `SpeakerRepository` trait double; that was dropped because `SpeakerRepository` is a concrete struct with inherent methods, not a trait, so a double requires the deferred `hexagonal-port-traits` refactor. The CHECK-constraint mechanism exercises the SAME atomicity guarantee against a real SQLite transaction, without a test double.); proportional-tail no-overlap fallback (proptest); re-diarize idempotency (**strict id-set equality + every split row's `token_timestamps IS NULL`** — NOT "non-decreasing", which would pass the exact NULL-tokens regression); empty / Unknown-only diarization; malformed source columns (`duration<=0`, `audio_start>audio_end`, NULL `meeting_id`); **malformed `token_timestamps` JSON** (crash-mid-write corruption deserialized by `align_with_tokens`); MIN_SPEECH_SECS floor; **CJK / no-whitespace proportional split** (whitespace splitter yields `words.len()==1` → 100% to one speaker); oversized token blob (~500 kB); SQLite host-param ceiling (N≈100); concurrent distinct sources (transactional isolation); same-key concurrency (two diarize paths on one meeting_id → guard holds). Property-based test invariants: word conservation per path, time-coverage **subset** (⊆ — diarization segments have gaps, so equality is unsatisfiable), duration recomputation, monotonic ordering, no empty rows, speaker containment — across both token and proportional paths, with a generator that produces gappy layouts.

**Smoke:** `frontend/e2e/smoke/diarization-speaker-split-persistence.spec.ts` — mock `diarization-complete` event **and** the transcripts re-fetch (the event payload carries no row data), emitting **before** any reload (post-reload `page.evaluate` emit is inert per project memory), asserting **distinct** speaker-badge text for a multi-speaker split (not just two badges).

## Open Questions

(none — all panel blockers resolved: framing corrected to conformance fix, mechanics made explicit, companion change extracted for boundary granularity.)

## Deferred

- **`DiarizationProcessor::process` unit test (task 2.3).** `process()` decodes audio inline (`decode_audio_file`, not behind `DiarizationPort`), so a mock-port test still requires a real audio file + DB pool — an integration test, not a unit test. The defensive wiring fix IS applied (it delegates to the same `persist_aligned_groups` helper covered by task 2.1's integration test). A full `process()` integration test is deferred to `hexagonal-port-traits`, which will port the decoder behind a trait and make a true unit test possible.
