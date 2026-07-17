## 1. Baseline regression guards (green BEFORE any deletion)

- [x] 1.1 Confirm or add a test asserting `stop_recording` (no transcription) leaves a `meetings` row — queue-discoverability invariant (D2). **Confirmed:** existing `sqlite_row_exists_after_save_with_correct_meeting_id` (`recording_commands.rs`) already calls `save_transcript(.., &[], ..)` and asserts the row.
- [x] 1.2 Add a test asserting the meeting folder contains `audio.mp4` after stop — proves the `recording_saver` audio role is independent of the segment role. **Resolution:** the segment touchpoints in `stop_and_save` are structurally separate from audio finalization (different fields/methods); the broader audio path is covered by the `recording_manager` `#[ignore]` real-device test. No lightweight unit test is feasible (needs `Runtime` + `IncrementalAudioSaver`).
- [x] 1.3 Add a test asserting that after a retranscription flow the meeting folder contains `transcripts.json` and the summary chain can read it — guards the auto-summary reader at `lib.rs`/`read_transcript_text` (D4). **Added:** `common_writer_feeds_summary_reader` round-trip test in `lib.rs` test module.

## 2. Delete the realtime worker + its Rust orphans (D1, D5)

- [x] 2.1 Deleted the **entire** `worker.rs` (every symbol in it was dead — `start_transcription_task`, `transcribe_chunk_with_provider`, `format_recording_time`, `TranscriptUpdate`, `reset_speech_detected_flag`, `SPEECH_DETECTED_EMITTED`). Removed `pub mod worker;` and the 3 re-exports from `transcription/mod.rs`.
- [x] 2.2 Deleted `reset_speech_detected_flag` + `SPEECH_DETECTED_EMITTED` (were in `worker.rs`) and their two call sites (`recording_commands.rs`), plus the `use` import.
- [x] 2.3 `TranscriptUpdate` had no live constructor (only a re-export chain + the dead `lib_old_complex.rs`); deleted with `worker.rs` and removed re-exports from `recording_commands.rs` and `audio/mod.rs`.
- [x] 2.4 Grep-verified: no live `emit(...)` for any worker event. Only matches were `lib_old_complex.rs` (deleted in §4) and a false-positive on the differently-named live `retranscription-progress` event.

## 3. Retire live callers of `recording_saver::TranscriptSegment`, THEN delete the segment role (D1, D3, D6)

- [x] 3.1 Deleted the `get_transcript_history` command + its `lib.rs` registration.
- [x] 3.2 Replaced the analytics `get_transcript_segments().len() as u64` with `0`.
- [x] 3.3 Removed the segment touchpoints in `stop_and_save` (transcripts.json save+verify block, duration fallback over `transcript_segments`, clear).
- [x] 3.4 Deleted the `from_recording_saver_segment_preserves_all_persisted_fields` test (and its now-empty `#[cfg(test)] mod tests`).
- [x] 3.5 Deleted `audio_processing::write_transcript_json_to_file`.
- [x] 3.6 Replaced stop-time segment gathering with `let segments: Vec<api::TranscriptSegment> = Vec::new();` — `save_transcript` still performs the `INSERT INTO meetings`.
- [x] 3.7 Deleted the segment role together: `recording_saver::TranscriptSegment` struct, `transcript_segments` field + init, all methods (`add_transcript_segment`, `add_transcript_chunk`, `get_transcript_segments`, the `write_transcripts_json` method), the `recording_manager` wrappers, and the `From` impl. **Baseline note:** on `main` the `From` impl had no `token_timestamps: None` line (that line arrives with the unmerged §2.2 diarization work) — the impl was a simpler 6-field conversion; deletion is identical either way.

## 4. Delete the uncompiled dead files

- [x] 4.1 Deleted `recording_saver_old.rs`.
- [x] 4.2 Deleted `lib_old_complex.rs`.

## 5. `transcripts.json` — delete only the dead writer (D4)

- [x] 5.1 Dead `recording_saver::write_transcripts_json` deleted in 3.7. Confirmed LIVE `common::write_transcripts_json` (`common.rs:60`, called from `retranscription.rs:704` + `import.rs:733`) is untouched.

## 6. Remove the dead channel plumbing (D7)

- [x] 6.1 Removed the `transcription_sender`/`transcription_receiver` channel: allocation in `recording_manager.rs`, params from `AudioPipeline::new` + `AudioPipelineManager::start`, the severed-`let _ =` drop, the `_transcription_receiver` bindings, and updated `start_recording` + `start_recording_with_defaults_and_auto_save` to return `Result<()>`. Cleaned the now-unused `AudioChunk` import.
- [x] 6.2 Rewrote `pipeline_does_not_initialise_vad` → `pipeline_runs_without_transcription_channel`: the transcription channel no longer exists, so the old VAD-forwarding assertion is vacuous; the new test guards the run-path completes cleanly on speech audio with the channel-free signature.

## 7. Spec sync + verification + follow-up issue

- [ ] 7.1 Before `/opsx:archive`, sync MODIFIED/REMOVED/ADDED requirements into canonical `whisper-model-selection` and `recording-lifecycle` (handled by the archive step).
- [x] 7.2 Grep-verified complete deletion — zero matches for any dead symbol in compiled sources.
- [x] 7.3 `cargo test --lib`: **485 passed; 0 failed; 16 ignored** (the `#[ignore]` real-audio/device tests). The diarization §5.1 token oracle is §2.2's work and is not on this `main`-based branch.
- [ ] 7.4 File the **frontend-orphan** follow-up GitHub issue (pending user authorization — visible-to-others action): the 3 `speech-detected` listeners (`recordingService.ts`, `RecordingControls.tsx`, `TranscriptView.tsx`), the `transcription-error` listener (`transcriptService.ts`) + any other worker-event listeners, the TS `TranscriptUpdate`/`TranscriptHistorySegment` types + `TranscriptContext` (incl. the `getTranscriptHistory` reload-sync path), the revive-speech-detected-from-VAD-vs-remove product decision, and the deferred `TranscriptionStatus`/`get_transcription_status` cleanup.
