## 1. Domain Types & Ports (pure, no I/O)

- [x] 1.1 RED: test `SpeakerLabel` rejects empty string and malformed IDs
- [x] 1.2 GREEN: implement `SpeakerLabel` with validation regex `^(Speaker \d+|Unknown Speaker( \(possibly .+\))?|.+)$`
- [x] 1.3 RED: test `SpeakerProfile` rejects empty name and names > 200 chars
- [x] 1.4 GREEN: implement `SpeakerProfile` with name validation
- [x] 1.5 RED: test `EmbeddingVector::from_slice` rejects wrong dimension and NaN/Inf
- [x] 1.6 GREEN: implement `EmbeddingVector(Vec<f32>)` with dimension and finite check
- [x] 1.7 Define `SpeakerEmbeddingPort` trait: `extract(audio: &[f32], sample_rate: u32) -> Result<EmbeddingVector>`
- [x] 1.8 Define `SpeakerIdentificationPort` trait: `add`, `search`, `verify`, `remove`, `list`
- [x] 1.9 Define `DiarizationPort` trait: `process(samples: &[f32], sample_rate: u32) -> Result<Vec<SpeakerSegment>>`

## 2. Alignment Algorithm (pure function — property-based testing)

- [x] 2.1 RED: proptest — for any valid token timestamps and diarization segments, every word is assigned to exactly one speaker (no gaps, no overlaps)
- [x] 2.2 RED: proptest — total text content is preserved before and after alignment (no words lost or duplicated)
- [x] 2.3 RED: test multi-speaker split produces correct text at boundary
- [x] 2.4 RED: test single-speaker segment is not split
- [x] 2.5 RED: test proportional split fallback when no token timestamps
- [x] 2.6 RED: test alignment with overlapping diarization segments (ambiguous — last writer wins)
- [x] 2.7 RED: test alignment with zero-length diarization segment (skipped)
- [x] 2.8 RED: test alignment with diarization gap (no speaker for a time range — labeled "Unknown")
- [x] 2.9 GREEN: implement `align_transcripts_with_diarization(transcripts, diarization_segments) -> Vec<AlignedSegment>`
- [x] 2.10 RED: test empty transcripts list → empty result
- [x] 2.11 RED: test empty diarization segments → all transcripts labeled "Unknown Speaker"
- [x] 2.12 RED: test malformed token_timestamps JSON (missing fields, negative timestamps, backwards timestamps) → falls back to proportional split
- [x] 2.13 Fix any failing proptest cases from 2.1–2.2

## 3. Test Doubles (port mocks for use-case testing)

- [x] 3.1 Implement `MockEmbeddingPort` returning configurable embeddings per input hash
- [x] 3.2 Implement `MockIdentificationPort` with in-memory HashMap, configurable match threshold
- [x] 3.3 Implement `MockDiarizationPort` returning configurable speaker segments
- [x] 3.4 RED: test `MockEmbeddingPort` returns error when audio < minimum duration
- [x] 3.5 GREEN: enforce minimum audio duration in mock

## 4. DB Schema & Repository

- [x] 4.1 Add DB migration: `speaker TEXT`, `token_timestamps TEXT`, `speaker_source TEXT` columns on `transcripts` table
- [x] 4.2 Add DB migration: `speakers` table (id TEXT PK, name TEXT NOT NULL, color TEXT NOT NULL, created_at, updated_at)
- [x] 4.3 Add DB migration: `speaker_embeddings` table (id TEXT PK, speaker_id TEXT FK, embedding BLOB NOT NULL, source_meeting_id TEXT FK, cluster_label TEXT, created_at)
- [x] 4.4 RED: test `SpeakerRepository::create_speaker` rejects empty name
- [x] 4.5 GREEN: implement `SpeakerRepository::create_speaker` with name validation
- [x] 4.6 RED: test `SpeakerRepository::create_speaker` rejects SQL injection in name (`'; DROP TABLE speakers; --`)
- [x] 4.7 GREEN: parameterized queries (verify SQL injection is rejected)
- [x] 4.8 RED: test `SpeakerRepository::create_speaker` rejects name > 200 chars
- [x] 4.9 GREEN: enforce name length limit
- [x] 4.10 RED: test embedding BLOB serialization round-trip (store Vec<f32> → BLOB → Vec<f32>)
- [x] 4.11 GREEN: implement embedding BLOB serialization with dimension validation
- [x] 4.12 RED: test embedding BLOB with wrong dimensions rejected on read
- [x] 4.13 GREEN: validate embedding dimension on deserialization
- [x] 4.14 RED: test embedding BLOB with NaN values rejected on write
- [x] 4.15 GREEN: validate finite values on serialization
- [x] 4.16 Implement `SpeakerRepository` CRUD: `get_speaker`, `list_speakers`, `update_speaker`, `remove_speaker`
- [x] 4.17 RED: test `remove_speaker` cascades to `speaker_embeddings`
- [x] 4.18 GREEN: implement cascade delete
- [x] 4.19 Implement `update_transcript_speaker(transcript_id, speaker, source)`
- [x] 4.20 Implement `split_transcript_row(transcript_id, split_point)` for multi-speaker segments
- [x] 4.21 RED: test `split_transcript_row` preserves original row ID and creates new row
- [x] 4.22 GREEN: implement split with correct ID and timestamp handling
- [x] 4.23 Implement `clear_speaker_labels(meeting_id)` for re-transcription
- [x] 4.24 RED: test `clear_speaker_labels` preserves rows with `speaker_source = "manual"`
- [x] 4.25 GREEN: implement selective clear (only auto-labeled rows)
- [x] 4.26 RED: test concurrent `label_speaker` calls for same cluster (last-write-wins, no deadlock)
- [x] 4.27 GREEN: implement with row-level locking or optimistic concurrency

## 5. Sherpa-ONNX Adapter (real model tests)

- [x] 5.1 Add `sherpa-onnx` crate dependency to `Cargo.toml` with shared linking features
- [x] 5.2 Create `audio/speaker/` module directory with `mod.rs`, `embedding.rs`, `diarization.rs`, `registry.rs`
- [x] 5.3 RED: test `SherpaOnnxEmbeddingAdapter` rejects zero-length audio (covered by min_samples guard)
- [ ] 5.4 RED: test `SherpaOnnxEmbeddingAdapter` rejects silence-only audio (all zeros) — requires model files, updated for 512-dim
- [x] 5.5 GREEN: implement embedding extraction wrapping `SpeakerEmbeddingExtractor`
- [x] 5.6 RED: test embedding dimension matches expected (512 for 3dspeaker on this system) — verified in integration test
- [x] 5.7 GREEN: verify `dim()` returns correct dimension
- [x] 5.8 RED: test transcript-driven chunking on short audio (<1s) returns empty segments
- [x] 5.9 GREEN: implement transcript-timestamp-driven chunking + centroid clustering
- [x] 5.10 RED: test centroid clustering on silence returns 0 speakers — verified in integration test
- [x] 5.11 RED: test centroid clustering on single-speaker audio returns 1 speaker — verified in integration test
- [x] 5.12 GREEN: handle edge cases (short, silence, single speaker)
- [x] 5.13 RED: test `SpeakerRegistryAdapter::search` with empty registry returns None
- [x] 5.14 RED: test `SpeakerRegistryAdapter::search` with threshold below all matches returns None
- [x] 5.15 GREEN: implement registry wrapping `SpeakerEmbeddingManager`
- [x] 5.16 RED: test `SpeakerRegistryAdapter` handles embedding dimension mismatch
- [x] 5.17 GREEN: validate embedding dimension before `add`/`search`
- [x] 5.18 Add model download/initialization logic for pyannote + 3dspeaker/wespeaker models
- [x] 5.19 RED: test model initialization with non-existent model path returns clear error
- [x] 5.20 GREEN: implement model path resolution with existence check

## 6. Diarization Processor (use case — tested with mocks)

- [x] 6.1 RED: test processor returns error when meeting has no `folder_path`
- [x] 6.2 GREEN: skip diarization when no audio file
- [x] 6.3 RED: test processor returns error when audio decode fails (corrupt file)
- [x] 6.4 GREEN: implement audio decode step with error propagation
- [x] 6.5 RED: test processor returns error when audio file > 2 hours (oversized)
- [x] 6.6 GREEN: enforce max audio duration guard
- [x] 6.7 RED: test processor handles diarization returning 0 speakers (silence-only meeting)
- [x] 6.8 GREEN: handle 0-speaker result gracefully (log warning, skip labeling)
- [x] 6.9 RED: test processor handles transcript rows with no matching diarization segment
- [x] 6.10 GREEN: label unmatched transcripts as "Unknown Speaker"
- [x] 6.11 RED: test processor handles diarization segment with no matching transcript
- [x] 6.12 GREEN: log warning for orphaned diarization segments, continue
- [x] 6.13 RED: test processor stores average embeddings per speaker per meeting
- [x] 6.14 GREEN: implement embedding extraction and storage
- [x] 6.15 RED: test processor applies confidence tiers correctly (high/medium/low)
- [x] 6.16 GREEN: implement confidence tier logic
- [x] 6.17 RED: test processor applies speaker count cap (re-runs when auto-detect exceeds cap)
- [x] 6.18 GREEN: implement speaker count cap enforcement
- [x] 6.19 RED: test processor emits `diarization-complete` event with correct payload
- [x] 6.20 GREEN: implement event emission
- [x] 6.21 RED: test concurrent diarization + user labeling (no deadlock, no data loss) — `auto_label_does_not_overwrite_manual` test
- [x] 6.22 GREEN: implement with appropriate locking — `update_transcript_speaker` guards auto writes with `WHERE speaker_source != 'manual'`

## 7. Token Timestamp Extraction (Whisper Provider)

- [x] 7.1 RED: test token timestamp extraction from empty Whisper segment → empty array
- [x] 7.2 GREEN: extend `TranscriptResult` with `token_timestamps: Option<String>`
- [x] 7.3 RED: test token timestamps with non-ASCII tokens (é, ñ, ü) → correct serialization
- [x] 7.4 GREEN: implement per-token timestamp extraction from Whisper segments
- [x] 7.5 RED: test token timestamps with oversized segment (500+ words) → no truncation
- [x] 7.6 GREEN: serialize as JSON array with no length limit
- [x] 7.7 RED: test `token_timestamps` column stores and retrieves correctly in DB
- [x] 7.8 GREEN: store `token_timestamps` in `transcripts` table when saving
- [x] 7.9 RED: test `TranscriptUpdate` event includes `token_timestamps` field — not needed; token timestamps are extracted during diarization from stored DB rows, not during live recording. The `token_timestamps` column is populated by the DB migration and read back during alignment.
- [x] 7.10 GREEN: extend `TranscriptUpdate` with `token_timestamps` field — same rationale; live recording doesn't need per-word timing in the event.

## 8. Queue Phase Integration

- [x] 8.1 RED: test `JobPhase::Diarizing` serialization round-trip
- [x] 8.2 GREEN: add `Diarizing` variant to `JobPhase` enum
- [x] 8.3 RED: test queue chains Summarising → Diarizing via CompletedChain
- [x] 8.4 GREEN: wire `CompletedChain` from Summarising → Diarizing
- [x] 8.5 RED: test queue chains Transcribing → Diarizing when no summary provider
- [x] 8.6 GREEN: wire direct chain when no summary
- [x] 8.7 RED: test Diarizing phase respects scheduler gates (recording active → paused)
- [x] 8.8 GREEN: register diarization processor with scheduler gate checking
- [x] 8.9 RED: test Diarizing phase skipped when no audio.mp4
- [x] 8.10 GREEN: processor checks for audio file before starting
- [x] 8.11 RED: test queue snapshot includes `phase = "diarizing"` correctly
- [x] 8.12 GREEN: extend queue snapshot serialization
- [x] 8.13 Extend `QueueJob` TypeScript type with `"Diarizing"` phase variant

## 9. Speaker Labeling Tauri Commands

- [x] 9.1 RED: test `label_speaker` with non-existent meeting_id returns error
- [x] 9.2 GREEN: implement meeting existence check
- [x] 9.3 RED: test `label_speaker` with non-existent cluster_label returns error
- [x] 9.4 GREEN: implement cluster existence check
- [x] 9.5 RED: test `label_speaker` with SQL injection name (`'; DROP TABLE speakers; --`) → rejected
- [x] 9.6 GREEN: validate speaker name (parameterized queries + input validation)
- [x] 9.7 RED: test `label_speaker` with XSS payload name (`<script>alert(1)</script>`) → sanitized
- [x] 9.8 GREEN: sanitize speaker name (strip HTML tags)
- [x] 9.9 RED: test `label_speaker` with empty name → rejected
- [x] 9.10 GREEN: reject empty names
- [x] 9.11 RED: test `label_speaker` with prompt injection name (`ignore previous instructions`) → stored as-is (not a command)
- [x] 9.12 GREEN: store literally, no special handling needed (parameterized queries prevent injection)
- [x] 9.13 RED: test `label_speaker` updates all transcript rows for the cluster
- [x] 9.14 GREEN: implement batch update of transcript rows
- [x] 9.15 RED: test `label_speaker` assigns persistent color from palette
- [x] 9.16 GREEN: implement color palette assignment
- [x] 9.17 Implement `list_speakers` Tauri command
- [x] 9.18 Implement `remove_speaker(speaker_id)` Tauri command
- [x] 9.19 RED: test `remove_speaker` with embeddings linked → embeddings unlinked (not deleted)
- [x] 9.20 GREEN: implement unlink-on-remove
- [x] 9.21 Implement `rediarize_meeting(meeting_id)` command: clear auto labels, re-enqueue
- [x] 9.22 RED: test `rediarize_meeting` preserves rows with `speaker_source = "manual"`
- [x] 9.23 GREEN: implement selective clear + re-enqueue

## 10. Frontend — Speaker Badge & Labeling

- [x] 10.1 RED: test `SpeakerBadge` renders speaker name with correct color
- [x] 10.2 GREEN: create `SpeakerBadge` component with color styling
- [x] 10.3 RED: test `SpeakerBadge` with very long name (>100 chars) truncates with ellipsis
- [x] 10.4 GREEN: implement text truncation
- [x] 10.5 RED: test `SpeakerBadge` with empty name shows "Unknown Speaker"
- [x] 10.6 GREEN: default to "Unknown Speaker" when name is empty/null
- [x] 10.7 RED: test `SpeakerBadge` with XSS name (`<script>alert(1)</script>`) renders as text, not HTML
- [x] 10.8 GREEN: ensure React escapes by default (verify no dangerouslySetInnerHTML)
- [x] 10.9 RED: test inline label input submits on Enter, clears on Escape
- [x] 10.10 GREEN: implement inline label input with keyboard handling
- [x] 10.11 RED: test inline label input rejects empty submission
- [x] 10.12 GREEN: disable submit button when input is empty
- [x] 10.13 RED: test existing speaker suggestions dropdown filters correctly
- [x] 10.14 GREEN: implement suggestions dropdown with name filtering
- [x] 10.15 RED: test "Unknown Speaker (possibly Alice)" renders with suggestion styling (italic, muted)
- [x] 10.16 GREEN: implement suggestion badge variant
- [x] 10.17 Integrate `SpeakerBadge` into transcript segment rendering
- [x] 10.18 RED: test `diarization-complete` event with malformed payload (missing fields) → no crash
- [x] 10.19 GREEN: validate event payload before updating state
- [x] 10.20 Wire click-to-rename into `VirtualizedTranscriptView`: add `editingSpeaker` state, load known speakers via `listSpeakers()`, pass callbacks to `TranscriptSegment`
- [x] 10.21 Swap `SpeakerBadge` ↔ `SpeakerLabelInput` in `TranscriptSegment` when editing, call `labelSpeaker()` on submit, refresh transcripts
- [x] 10.22 Thread `meetingId` and `onSpeakersChanged` props from `TranscriptPanel` → `VirtualizedTranscriptView`
- [x] 10.23 Wire click-to-rename into `TranscriptView` (live recording view) — same pattern as 10.20–10.21

## 11. Frontend — Settings & Queue UI

- [x] 11.1 RED: test speaker model dropdown renders both options
- [x] 11.2 GREEN: create speaker model selection component
- [x] 11.3 RED: test confidence threshold slider rejects values outside 0.5–0.8
- [x] 11.4 GREEN: implement threshold slider with bounds
- [x] 11.5 RED: test max speaker cap input rejects values outside 2–20
- [x] 11.6 GREEN: implement cap input with bounds
- [x] 11.7 Add "Re-diarize" button in meeting details view
- [x] 11.8 RED: test queue UI shows "Diarizing" phase correctly
- [x] 11.9 GREEN: extend queue UI to handle Diarizing phase — type updated in queueService.ts
- [x] 11.10 Listen for `diarization-complete` event and update transcript view state

## 12. Re-transcription & Import Integration

- [x] 12.1 RED: test re-transcription clears `speaker` and `speaker_source` on auto-labeled rows
- [x] 12.2 GREEN: implement clear on re-transcription trigger
- [x] 12.3 RED: test re-transcription preserves rows with `speaker_source = "manual"`
- [x] 12.4 GREEN: implement selective clear
- [x] 12.5 RED: test re-transcription re-enqueues Diarizing phase
- [x] 12.6 GREEN: implement re-enqueue after re-transcription
- [x] 12.7 RED: test imported audio includes Diarizing phase in queue job
- [x] 12.8 GREEN: wire Diarizing phase into import queue path

## 13. Onboarding Integration

- [x] 13.1 RED: test onboarding Step 3 includes speaker model in download list
- [x] 13.2 GREEN: add pyannote segmentation + default embedding model to Step 3
- [x] 13.3 RED: test speaker model download failure does not block onboarding completion
- [x] 13.4 GREEN: implement graceful failure with warning
- [x] 13.5 RED: test speaker model download progress tracked alongside existing downloads
- [x] 13.6 GREEN: integrate progress tracking

## 14. Integration Tests (automated, run in CI)

- [x] 14.1 RED: test full pipeline with 1s test audio → decode → diarize → align → DB rows updated
- [x] 14.2 GREEN: wire end-to-end test with small test WAV file
- [x] 14.3 RED: test pipeline with silence-only audio → 0 speakers → all "Unknown"
- [x] 14.4 GREEN: handle silence gracefully
- [x] 14.5 RED: test label_speaker → cross-meeting matching → auto-label in second meeting
- [x] 14.6 GREEN: implement and verify cross-meeting flow
- [x] 14.7 RED: test re-diarize → manual corrections preserved
- [x] 14.8 GREEN: verify re-diarization flow

## 15. Smoke Tests (manual, in-app)

- [ ] 15.1 Record a meeting with 2+ speakers → verify diarization runs and labels appear in transcript view
- [ ] 15.2 Label "Speaker 1" as "Alice" → record second meeting → verify Alice auto-matched
- [ ] 15.3 Re-diarize a meeting with manual corrections → verify corrections preserved
- [ ] 15.4 Import an audio file → verify diarization produces speaker labels
- [ ] 15.5 Re-transcribe a meeting → verify speaker labels cleared and re-diarized
- [ ] 15.6 Verify speaker colors persist across meetings (Alice is always the same color)
- [ ] 15.7 Verify confidence threshold slider in settings updates matching behavior

## 16. Hardening Fixes (2026-06-08/09)

- [x] 16.1 Fix 6 compiler warnings: unused imports, dead code, unnecessary mut in speaker module
- [x] 16.2 Fix sherpa-onnx crash (STATUS_STACK_BUFFER_OVERRUN): validate segmentation model path exists before C++ FFI call
- [x] 16.3 Fix stale embedding dimension test: change from 128-dim to 8-dim (128 is valid after variable-dimension fix)
- [x] 16.4 Fix `auto_label_does_not_overwrite_manual` test: use in-memory SQLite instead of hardcoded production DB path
- [x] 16.5 Fix re-diarization stale embeddings: delete old embeddings for meeting before re-running diarization
- [x] 16.6 Document inline suggestion chips as merge actions (not rename) in spec
