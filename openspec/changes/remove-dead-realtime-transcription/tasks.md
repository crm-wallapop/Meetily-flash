## 1. Baseline regression guards (green BEFORE any deletion)

- [ ] 1.1 Confirm or add a test asserting `stop_recording` (no transcription) leaves a `meetings` row — queue-discoverability invariant (D2). Reuse the `transcription_queue.rs` coverage if present.
- [ ] 1.2 Add a test asserting the meeting folder contains `audio.mp4` after stop — proves the `recording_saver` audio role is independent of the segment role.
- [ ] 1.3 Add a test asserting that after a retranscription flow the meeting folder contains `transcripts.json` and the summary chain can read it — guards the auto-summary reader at `lib.rs:764`/`read_transcript_text` (D4). (The existing retranscription/import token tests use `common::TranscriptSegment` and are unaffected; they stay green as a baseline too.)

## 2. Delete the realtime worker + its Rust orphans (D1, D5)

- [ ] 2.1 Delete `start_transcription_task` and its private helpers from `worker.rs`. Run `cargo check`; follow the compiler to remove unused symbols/re-exports across `transcription/mod.rs:20-24`, `recording_commands.rs:29`, `audio/mod.rs:92`.
- [ ] 2.2 Delete `reset_speech_detected_flag` and the `SPEECH_DETECTED_EMITTED` static (`worker.rs:15-21`) AND their two live call sites (`recording_commands.rs:346`, `:488`). The only setter (`worker.rs:177`) dies with the worker, so these become log-noise noops — backend Rust, in-scope (D5). Remove the `pub use` at `transcription/mod.rs`.
- [ ] 2.3 If `TranscriptUpdate` (`worker.rs:24`) has no live constructor after 2.1, delete it and its re-exports. First grep `audio::TranscriptUpdate` / `crate::audio::TranscriptUpdate` across tests/examples.
- [ ] 2.4 Grep-verify NO live `emit(...)` remains for any worker-emitted event: `transcript-update`, `speech-detected`, `transcription-error`, `transcription-progress`, `transcription-queue-complete`, `transcript-chunk-loss-detected`. (The frontend `transcription-error` listener at `transcriptService.ts:65` is a deferred orphan — task 8.4.)

## 3. Retire live callers of `recording_saver::TranscriptSegment`, THEN delete the segment role (D1, D3, D6)

Each step keeps `cargo check` green: callers are retired first, then the struct + field + all methods + `From` impl are removed together in 3.7 (no intermediate state where the field is gone but methods still reference it).

- [ ] 3.1 Delete the `get_transcript_history` Tauri command (`recording_commands.rs:1007`) and its registration (`lib.rs:1147`) — return type vanishes with the struct; functionally dead (always `[]`). Frontend caller (`transcriptService.getTranscriptHistory` → `TranscriptContext.tsx:252`) → follow-up issue (8.4).
- [ ] 3.2 Replace `recording_commands.rs:579` `mgr.get_transcript_segments().len() as u64` with `0` in the analytics snapshot (transcript count is always 0 post-meeting).
- [ ] 3.3 In `stop_and_save` (`recording_saver.rs:334`), remove the segment touchpoints: the dead `self.write_transcripts_json(folder)` call (`:378`), the `transcripts.json` existence check (`:384-388`), the `transcript_segments.lock()` duration fallback (`:400`), and the clear (`:431`). Keep the audio finalization. (Do NOT remove the `transcript_segments` field here — that happens in 3.7 with the methods.)
- [ ] 3.4 Delete the `from_recording_saver_segment_preserves_all_persisted_fields` test (`api.rs:1415`) — it constructs the struct under `#[cfg(test)]`.
- [ ] 3.5 Delete `audio_processing::write_transcript_json_to_file` (`audio_processing.rs:734`) — param-typed on the struct; its only caller `recording_saver_old.rs:348` is uncompiled (deleted in §4).
- [ ] 3.6 Replace the stop-time segment gathering + save (`recording_commands.rs:790-797`) with a meeting-row-only call: stop binding `segments` from `get_transcript_segments()`, pass an empty list (or add a `TranscriptsRepository::create_meeting_row` variant if it cleans the API). Preserve the `INSERT INTO meetings` (`transcript.rs:59`). This is a single edit — the gathering and the save call are one site. (Satisfies D2.)
- [ ] 3.7 NOW delete the segment role together: the `recording_saver::TranscriptSegment` struct, the `transcript_segments` field (`recording_saver.rs:55`, init `:67`), ALL its methods (`add_transcript_segment`, `add_transcript_chunk`, `get_transcript_segments`, and the `write_transcripts_json` method at `:270`), the `recording_manager.rs` wrappers (`:452/:458/:463`), and the `From<recording_saver::TranscriptSegment>` impl (`api.rs:197`, the `token_timestamps: None` hardcode). By this step every caller is retired, so `cargo check` is green.

## 4. Delete the uncompiled dead files

- [ ] 4.1 Delete `frontend/src-tauri/src/audio/recording_saver_old.rs` (already absent from `audio/mod.rs`).
- [ ] 4.2 Delete `frontend/src-tauri/src/lib_old_complex.rs` — no `mod` declaration exists in `lib.rs` to remove (it is uncompiled today).

## 5. `transcripts.json` — delete only the dead writer (D4)

- [ ] 5.1 The dead `recording_saver::write_transcripts_json` method was deleted in 3.7. Confirm the LIVE `common::write_transcripts_json` (`common.rs:60`, called from `retranscription.rs:705` + `import.rs:733`) is untouched — it feeds the summary reader at `lib.rs:764`. No reader migration. Guard 1.3 covers this.

## 6. Remove the dead channel plumbing (D7)

- [ ] 6.1 Remove the `transcription_sender`/`transcription_receiver` channel: the allocation in `recording_manager.rs:82`, the sender parameter threaded into `AudioPipeline::new` (`pipeline.rs:707`, dropped at `:726`) and `AudioPipeline::run` plumbing, and the `_transcription_receiver` bindings at `recording_commands.rs:311`/`:456`. Update `start_recording`'s signature (it no longer returns the receiver).
- [ ] 6.2 Reconcile the `#[cfg(test)]` red test `pipeline_does_not_initialise_vad` (`pipeline.rs:1010`) — it is keyed to `audio-recording-quality` task 1.2; update its assertion/comment or leave a cross-reference. Do not silently break it.

## 7. Spec sync + verification + follow-up issue

- [ ] 7.1 Before `/opsx:archive`, re-read canonical `whisper-model-selection` and `recording-lifecycle` against this change's delta; sync MODIFIED/REMOVED/ADDED requirements into the canonical specs.
- [ ] 7.2 Grep-verify complete deletion: no `start_transcription_task`, `recording_saver::TranscriptSegment`, `From<recording_saver::TranscriptSegment>`, `reset_speech_detected_flag`, `SPEECH_DETECTED_EMITTED`, `recording_saver_old`, `lib_old_complex`, `transcription_sender`/`_transcription_receiver`, `write_transcript_json_to_file` in compiled sources.
- [ ] 7.3 Full gate green: `cargo test`. Re-run the diarization §5.1 `#[ignore]` real-audio oracle to confirm the live token path is unregressed.
- [ ] 7.4 File the **frontend-orphan** follow-up GitHub issue: the 3 `speech-detected` listeners (`recordingService.ts`, `RecordingControls.tsx`, `TranscriptView.tsx`), the `transcription-error` listener (`transcriptService.ts:65`) + any other worker-event listeners surfaced in 2.4, the TS `TranscriptUpdate`/`TranscriptHistorySegment` types + `TranscriptContext` (incl. the `getTranscriptHistory` reload-sync `useEffect` at `:252`), and the revive-speech-detected-from-VAD-vs-remove product decision. Also note the deferred `TranscriptionStatus`/`get_transcription_status` cleanup (partially live — `is_processing` derives from the queue phase).
