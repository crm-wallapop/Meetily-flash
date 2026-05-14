## Why

Live transcription during recording (VAD → 50ms chunks → Whisper) produces hallucinated garbage. A controlled capture in `meetily-recordings/Meeting 2026-05-13_21-11-11_2026-05-13_19-11/transcripts.json` confirmed this: 1.81 s of audio produced a single segment of mixed-Unicode noise (`"I幫 IPp actor daw throws Coming Aim пош отд..."`). The failure mode is structural: Whisper receives isolated noise bursts with no surrounding context, no ability to suppress them. The existing `retranscription.rs` module already implements a superior path: it decodes the saved MP4, runs VAD over the full file with 2 s redemption windows, and gives Whisper contiguous audio context that suppresses hallucinations.

Making `retranscription.rs` the *primary* path — triggered automatically after every recording — removes the live transcription complexity, delivers better accuracy, and resolves a secondary problem the user flagged: back-to-back meetings. With live transcription gone, `stop_recording` finalises the MP4 in under a second and a new recording can start immediately while the previous one's transcription runs in the background.

Two safety guarantees go in the same change so the new background pipeline never becomes a silent performance killer: a scheduling layer pauses transcription when the system is busy (you are recording, in a Meet/Zoom call, or under sustained CPU/RAM load), and a global "Pause all background work" override gives the user direct control. Configurable thresholds for the scheduler are deferred to a follow-up change (`transcription-scheduler-advanced`); this change ships sensible hardcoded defaults.

## What Changes

- **BREAKING**: Remove the live VAD → Whisper transcription path from `pipeline.rs`; the pipeline records to MP4 only during a session. No `transcription_mode` opt-back toggle — the live panel is unused and the live output is unusable.
- **BREAKING**: Remove the `recover_audio_from_checkpoints` Tauri command and the per-chunk `.ckpt` files written during recording. The MP4 itself is the recovery artefact.
- Auto-trigger transcription on the saved MP4 immediately after `stop_recording` finalises the file. Chain LLM summarisation when a provider is configured.
- Add a transcription job scheduler (Rust-side) that gates job execution on system state:
  - **Recording active** (read from `RECORDING_PHASE` atomic from `fix-stop-responsiveness`)
  - **Meeting application detected** (read from existing `meeting_detection`)
  - **CPU above threshold sustained for a window** (hardcoded: 70 % for 30 s in this change)
  - **RAM above threshold sustained for a window** (hardcoded: 80 % for 30 s in this change)
- Pause granularity: a paused job finishes its current Whisper chunk then yields; resumes once all gates clear.
- The same scheduler governs LLM summarisation jobs — one policy, one pause button.
- Repurpose IndexedDB (`indexedDBService.ts`) as the **transcription queue persistence layer**. Schema: `{ meetingId, status: 'pending'|'in_progress'|'paused'|'done'|'failed', queuePosition, pauseReason?, startedAt?, completedAt?, lastError? }`. Stops writing live-transcript chunks; queue state persists across crashes.
- Repurpose the existing transcript-recovery modal (`useTranscriptRecovery.ts`): on startup, scan IndexedDB for `pending` or `in_progress` jobs and offer to resume them. Replace the IndexedDB-chunks-plus-checkpoint-reassembly flow with a simpler MP4-already-on-disk flow.
- Replace the live transcript panel during recording with a static message: "Recording — transcript will be generated after you stop."
- Add a queue UI surface:
  - **Per-meeting state** inline in the meeting view: `Transcribing 34%` / `Queued #2` / `Paused — you're recording` / `Paused — high CPU` / `Done` / `Failed`.
  - **Global queue indicator** in the app shell: `N transcriptions queued (paused)` with a "Pause all" / "Resume" toggle.

## Capabilities

### New Capabilities
- `post-meeting-pipeline`: Describes the auto-triggered batch transcription → summary chain, the scheduler gates that govern when jobs run, the queue state machine, and the recovery flow on app restart.

### Modified Capabilities
- `audio-recording-quality`: Recording pipeline no longer runs VAD or Whisper during capture; the pipeline's sole output during a session is the MP4 file. No `transcription_mode` qualifier — the change applies unconditionally.

## Impact

- `frontend/src-tauri/src/audio/pipeline.rs` — remove the VAD / Whisper live path
- `frontend/src-tauri/src/audio/recording_manager.rs` — trigger the transcription queue after `finalize()`
- `frontend/src-tauri/src/audio/retranscription.rs` — add an auto-mode entry point that obeys the scheduler; chain summary trigger on completion
- `frontend/src-tauri/src/audio/incremental_saver.rs` — remove `.ckpt` writes (only the final MP4 is needed)
- `frontend/src-tauri/src/use_cases/` — new `transcription_queue.rs` use case (scheduler + queue state machine)
- `frontend/src-tauri/src/use_cases/recording_gc.rs` — extend to clean up stale queue rows
- `frontend/src/services/indexedDBService.ts` — replace transcript-chunk schema with queue-state schema
- `frontend/src/hooks/useTranscriptRecovery.ts` — change source of recoverable meetings (IndexedDB queue table instead of transcript chunks); remove `recover_audio_from_checkpoints` calls
- `frontend/src/contexts/TranscriptContext.tsx` — stop subscribing to live `transcript-update` events; subscribe to queue state events
- `frontend/src/` — replace live transcript panel with the post-recording progress view; add per-meeting state badges and the app-shell queue indicator
- Removes Tauri commands: `recover_audio_from_checkpoints`, `has_audio_checkpoints`, `cleanup_checkpoints`
- Adds Tauri commands: `pause_all_background_work`, `resume_all_background_work`, `get_queue_state`, `cancel_queued_job`
- Adds Tauri events: `transcription-queue-changed` (queue snapshot whenever any job state changes)

## Out of Scope (deferred to `transcription-scheduler-advanced`)

- Configurable scheduling thresholds (CPU %, RAM %, sustained-duration windows)
- Settings UI under `Advanced > Background processing`
- Scheduling mode preset (`aggressive` / `polite` / `manual`)
- GPU-load gate (deferred until system-load gates are measured in practice)

These ship with hardcoded defaults in this change so the safety guarantees land immediately; the follow-up exposes them to user control.
