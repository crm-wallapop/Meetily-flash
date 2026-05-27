## Why

Meetings frequently involve multiple participants, but the current transcript has no speaker attribution. Users cannot tell who said what — making transcripts less useful for action-item assignment, summarization accuracy, and post-meeting review. Consistent speaker identification across meetings (recognising "Alice" from one meeting to the next) is the key differentiator between a raw transcript and a genuinely useful meeting record.

## What Changes

- Add `sherpa-onnx` crate dependency for offline speaker diarization and embedding extraction.
- New `Diarizing` phase in the transcription queue, chained after `Summarising` (or directly after `Transcribing` if no summary provider is configured).
- Store per-word token timestamps from Whisper in a new `token_timestamps` column on the `transcripts` table, enabling precise alignment between diarization speaker boundaries and transcript text.
- Add `speaker` column to `transcripts` table (nullable, `"Speaker 1"`, `"Speaker 2"`, etc., or a named label).
- New SQLite tables: `speakers` (id, name, color, created_at, updated_at) and `speaker_embeddings` (id, speaker_id, embedding BLOB, source_meeting_id, created_at).
- Embedding-based cross-meeting speaker matching: when a user labels "Speaker 1" as "Alice", the embedding is stored; future meetings auto-match if similarity exceeds the confidence threshold.
- Confidence tiers: high-confidence matches show the speaker name; lower-confidence matches show "Unknown Speaker (possibly Alice)".
- Retroactive labeling: inline badge UI in transcript view to label speakers post-recording.
- Per-speaker persistent colors across all meetings.
- "Re-diarize" button in meeting details that re-runs diarization while protecting manually corrected labels.
- Embedding models (3dspeaker ~26 MB, wespeaker ~90 MB) user-selectable in settings; pyannote segmentation model (~50 MB) included as required onboarding download.
- Exposed confidence threshold in advanced settings.
- Diarization also runs on imported audio files; re-transcription clears labels and re-enqueues diarization.

## Capabilities

### New Capabilities

- `speaker-diarization`: Offline speaker diarization, embedding extraction, cross-meeting speaker matching, retroactive labeling, and queue integration.

### Modified Capabilities

- `recording-lifecycle`: `TranscriptSegment` and `TranscriptUpdate` gain optional `speaker` field; `background_shutdown` / queue emits `diarization-complete` event.
- `post-meeting-pipeline`: Queue gains `Diarizing` phase after `Summarising`; `JobPhase` enum extended.
- `whisper-model-selection`: Whisper provider stores token timestamps; `TranscriptResult` extended with optional token timestamp data.

## Impact

- **Rust (Tauri app)**: New crate (`sherpa-onnx`), new ports (`SpeakerEmbeddingPort`, `SpeakerIdentificationPort`), new adapter (`SherpaOnnxSpeakerAdapter`), modified queue worker, modified Whisper provider, new Tauri commands (`label_speaker`, `list_speakers`, `remove_speaker`, `rediarize_meeting`), DB migration.
- **Frontend (React/TS)**: New inline speaker badge components, speaker label input, "re-diarize" button, speaker settings section, confidence threshold slider, color assignment logic.
- **Database**: Schema migration adding `speaker` column to `transcripts`, `token_timestamps` column to `transcripts`, new `speakers` and `speaker_embeddings` tables.
- **Onboarding**: Step 3 downloads segmentation model (~50 MB) alongside existing Parakeet/Gemma downloads.
- **Models**: `sherpa-onnx` bundles prebuilt static libraries for Windows x64, macOS arm64/x64, Linux x64/aarch64. Static linking, Apache-2.0 license.
