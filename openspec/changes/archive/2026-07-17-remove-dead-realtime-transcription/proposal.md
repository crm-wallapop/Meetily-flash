## Why

Realtime (transcribe-as-you-record) transcription is no longer supported. Transcription runs post-meeting from the saved audio file — the recording stop flow enqueues a transcription job (`useRecordingStop.ts` → `enqueue_transcription_job`), and the queue's `start_retranscription` does the actual Whisper pass. Imports go through `import.rs`. Both already populate `token_timestamps`.

The old realtime path is now dead code with no caller: `start_transcription_task` (`worker.rs:42`) is referenced only by its own definition and a `pub use`. `pipeline.rs:685` and `:724-726` state outright that "transcription runs post-meeting from the saved audio file," and the producer is already severed at `pipeline.rs:726` (`let _ = transcription_sender;`). `audio-recording-quality` spec line 178 already forbids Whisper inference and `transcript-update` emission during recording.

Dead code that describes a path that no longer exists is actively harmful: it caused a real misdiagnosis this week — a "live recording token gap" was filed against `worker.rs:414` discarding token data, when in fact live recordings are transcribed by `start_retranscription` (which already binds tokens). The investigation cost a full session. Removing the dead path prevents the next reader from making the same mistake.

## What Changes

Backend-only deletion of the unsupported realtime transcription path and the dead code it orphans:

- Delete `start_transcription_task` and the private helpers only it calls (`worker.rs`), plus types/re-exports that become unused (compile-checked, with live callers pre-enumerated in the design).
- Delete `reset_speech_detected_flag` + the `SPEECH_DETECTED_EMITTED` static and their two live call sites (`recording_commands.rs:346`, `:488`). These are backend Rust; the only flag-setter (`worker.rs:177`) dies with the worker, so they become log-noise noops. In scope here, not deferred.
- Delete the `recording_saver::TranscriptSegment` persistence chain: the struct, `add_transcript_segment` / `add_transcript_chunk` / `get_transcript_segments`, the `transcript_segments` field and its touchpoints inside `stop_and_save`, and the `From<recording_saver::TranscriptSegment>` impl at `api.rs:197` (the `token_timestamps: None` hardcode).
- Retire the live callers of that struct first: delete the functionally-dead `get_transcript_history` Tauri command (`recording_commands.rs:1007`); replace the analytics `get_transcript_segments().len()` at `recording_commands.rs:579` with `0`; delete the `From`-impl unit test (`api.rs:1415`) and `audio_processing::write_transcript_json_to_file` (`audio_processing.rs:734`).
- Delete the dead `transcripts.json` writer (`recording_saver::write_transcripts_json`, fed by the always-empty segment list). The LIVE `common::write_transcripts_json` (called by retranscription + import) is untouched and continues to feed the auto-summary reader at `lib.rs:764`.
- Remove the dead `transcription_sender` / `_transcription_receiver` channel plumbing in `recording_manager.rs`, `pipeline.rs`, and `recording_commands.rs` — it exists only to feed the deleted worker.
- Delete the two uncompiled dead files: `recording_saver_old.rs`, `lib_old_complex.rs`.
- Simplify the stop-recording `save_transcript` call (`recording_commands.rs:797`) to meeting-row creation only (it still owns the `INSERT INTO meetings` at `transcript.rs:59` that the queue depends on).
- Correct the spec lies (see Capabilities): two canonical specs claim a `TranscriptUpdate` Tauri event carries token/speaker data during recording. No such event fires.

**Explicitly NOT removed (live, unchanged):** the `token_timestamps` DB column and its binds (`retranscription.rs:687`, `import.rs:849`, `transcript.rs:88`); `api::TranscriptSegment.token_timestamps`; the domain `common::TranscriptSegment`; the live `common::write_transcripts_json` + the summary reader; diarization's token reader; `MeetingMetadata.transcript_file` (still accurate — `transcripts.json` is still written by `common::`). No schema migration.

**Deferred to a follow-up issue (frontend, out of scope here):** the three `speech-detected` frontend listeners, the `transcription-error` listener and any other worker-event listeners, the TypeScript `TranscriptUpdate` / `TranscriptHistorySegment` types and `TranscriptContext` (incl. the `getTranscriptHistory` reload-sync path), and the revive-speech-detected-from-VAD-vs-remove product decision. Also deferred: `TranscriptionStatus` / `get_transcription_status` (partially live — `is_processing` derives from the queue phase).

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `whisper-model-selection`: reframe the "stores token timestamps" requirement off the dead realtime worker and `TranscriptUpdate` event onto the live post-meeting retranscription and import save paths. The DB-storage mandate is unchanged.
- `recording-lifecycle`: drop the scenarios that describe a `TranscriptUpdate` event emitted during recording (no such event fires). Replace with a `TranscriptSegment carries an optional speaker field` requirement that preserves the live `diarization-complete` scenario.

## Impact

- **Code**: `audio/transcription/worker.rs`, `audio/recording_saver.rs`, `audio/recording_commands.rs`, `audio/recording_manager.rs`, `audio/audio_processing.rs`, `audio/pipeline.rs`, `audio/mod.rs`, `audio/transcription/mod.rs`, `api/api.rs`, `lib.rs`; deletion of `audio/recording_saver_old.rs` and `lib_old_complex.rs`.
- **No schema migration**: the `token_timestamps` column and all live binds are untouched.
- **No user-visible behavioral change**: the deleted code never executed; transcription was already post-meeting. The already-broken `speech-detected` indicator stays broken pending the follow-up.
- **Spec honesty**: two requirements stop describing an event that does not fire.
