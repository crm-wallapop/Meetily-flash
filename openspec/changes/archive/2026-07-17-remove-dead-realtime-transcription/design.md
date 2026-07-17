## Context

The app has two transcription entry points, both post-recording:

```
RECORDING (capture only)
  start_recording → pipeline captures+mixes audio, writes audio.mp4 via recording_saver
  stop_recording  → save_recording_only (audio.mp4) + save_transcript (INSERT meeting row)
  frontend        → enqueue_transcription_job(meeting_id, audio_path)
        │
        ▼
QUEUE → start_retranscription (retranscription.rs)
          transcribe_audio_with_confidence → (text, conf, is_partial, token_ts)
          create_transcript_segments (domain common::TranscriptSegment, carries token_ts)
          common::write_transcripts_json (LIVE sidecar writer)
          DELETE + INSERT transcripts … .bind(token_timestamps)   ← LIVE token path

IMPORT (import.rs)
  common::write_transcripts_json → INSERT transcripts … .bind(token_timestamps)  ← LIVE token path
```

The dead realtime path is a third, never-driven branch:

```
start_transcription_task (worker.rs:42)   ← NO CALLER (verified)
  receives AudioChunk → transcribe_chunk_with_provider → emits transcript-update / speech-detected / …
  → recording_saver::add_transcript_segment → get_transcript_segments
```

The producer side is already severed: `pipeline.rs:726` does `let _ = transcription_sender;` ("transcription now runs post-meeting"), so no audio ever flows to the worker and nothing is silently dropped. The realtime path is dead at both ends.

`recording_saver.rs` has **two independent roles**: (1) audio saving (`IncrementalAudioSaver`, `stop_and_save`/`save_recording_only`, writing `audio.mp4` + `metadata.json`) — LIVE; (2) transcript-segment storage (`TranscriptSegment` struct, `transcript_segments` field, `add_transcript_segment`, `add_transcript_chunk`, `get_transcript_segments`, the `write_transcripts_json` method) — DEAD. This change removes role (2) and leaves role (1) intact.

## Goals / Non-Goals

**Goals**
- Delete every backend line of the unsupported realtime transcription path, transitively, so no reader mistakes it for live.
- Remove the backend Rust dead code that the worker deletion orphans (`reset_speech_detected_flag` + `SPEECH_DETECTED_EMITTED`, the `transcription_sender`/`_transcription_receiver` channel plumbing).
- Keep the meeting-row creation on stop; keep the `token_timestamps` column and every live bind; keep the live `transcripts.json` reader (auto-summary) fed by the live `common::` writer.
- Make the canonical specs stop claiming a `TranscriptUpdate` event that does not fire.

**Non-Goals**
- Frontend orphan cleanup — the three `speech-detected` listeners (`recordingService.ts`, `RecordingControls.tsx`, `TranscriptView.tsx`), the `transcription-error` listener (`transcriptService.ts:65`), the TypeScript `TranscriptUpdate`/`TranscriptHistorySegment` types, `TranscriptContext`, and `transcriptService.getTranscriptHistory()` are deferred to a follow-up issue.
- The `TranscriptionStatus` type / `get_transcription_status` command — partially live (`is_processing` derives from the queue phase); deferred with rationale rather than ripped out.
- Any change to the live transcription paths (queue retranscription, import).
- Reviving the speech-detected indicator from pipeline VAD (a feature, not a cleanup).

## Decisions

### D1 — Compile-driven deletion, but with pre-enumerated live callers

Delete `start_transcription_task` first; `cargo check` then enumerates the cascade. But the implementer is given the live callers up front (the blind cascade leaves the right resolution to guesswork):

| Live caller of a to-be-deleted symbol | Resolution |
|---|---|
| `get_transcript_history` Tauri command (`recording_commands.rs:1007`, returns `Vec<recording_saver::TranscriptSegment>`, registered `lib.rs:1147`, frontend-called) | Delete the Rust command (D6); frontend caller deferred |
| `recording_commands.rs:579` `get_transcript_segments().len() as u64` in the analytics snapshot | Replace with `0` (transcript count is always 0 post-meeting) |
| `stop_and_save` internals (`recording_saver.rs:378` dead writer call, `:400` duration fallback over `transcript_segments`, `:431` clear) + the `transcript_segments` field (`:55`, init `:67`) | Remove the touchpoints; `stop_and_save` keeps its audio role |
| `api.rs:1415` test `from_recording_saver_segment_preserves_all_persisted_fields` (constructs the struct under `#[cfg(test)]`) | Delete with the struct |
| `audio_processing.rs:734 write_transcript_json_to_file` (param-typed on the struct; only caller `recording_saver_old.rs:348`) | Delete with the dead files |

These are handled in tasks §3 **before** the struct + `From` impl are removed, so `cargo check` stays green at each step rather than failing mid-cascade.

### D2 — `save_transcript` on stop stays, for the meeting row only

`TranscriptsRepository::save_transcript` performs both `INSERT INTO meetings` (`transcript.rs:59`) and `INSERT INTO transcripts` (`:88`). `start_retranscription` does NOT insert the meeting row (no `INSERT INTO meetings` in retranscription.rs), so the stop-time call is load-bearing — the queue cannot discover the recording without it. The call stays; only the always-empty segment-gathering is dropped.

### D3 — The hardcode disappears with the struct, it is not "fixed"

The `token_timestamps: None` at `api.rs:206` is removed by deleting the `From<recording_saver::TranscriptSegment>` impl together with the struct. There is no live consumer once the stop site stops gathering segments. We do not add a field and wire it — that was the mistaken premise of the abandoned `live-recording-token-timestamps` change.

### D4 — `transcripts.json`: three writers, the live reader is fed by `common::`, no migration

`transcripts.json` has three writers; only the design of this change requires deleting one:

| Writer | Location | Fed by | Status |
|---|---|---|---|
| `recording_saver::write_transcripts_json` (method) | `recording_saver.rs:270`, called from `stop_and_save:378` | `self.transcript_segments` (always empty) | DEAD — delete |
| `common::write_transcripts_json` (free fn) | `common.rs:60`, called from `retranscription.rs:705` + `import.rs:733` | freshly-transcribed domain segments | LIVE — keep |
| `audio_processing::write_transcript_json_to_file` | `audio_processing.rs:734`, only caller `recording_saver_old.rs:348` | the deleted struct | DEAD — delete |

The live reader is `read_transcript_text` (`lib.rs:500`) → the queue's `summary_processor` (`lib.rs:764`), which feeds `SummaryService::process_transcript_background`. It reads `transcripts.json` AFTER `start_retranscription` has overwritten it via the LIVE `common::` writer. Deleting the dead `recording_saver` writer does **not** starve the reader. **No reader migration occurs** — an earlier draft of this design conflated the two writers and would have guided an implementer to break the auto-summary chain. (Filename note: the artifact is `transcripts.json`, plural; `metadata.json`, not `meeting_metadata.json`.)

### D5 — `reset_speech_detected_flag` + `SPEECH_DETECTED_EMITTED` are removed in THIS change

These are backend Rust (`worker.rs:15` static, `:18` function), called from live Rust (`recording_commands.rs:346`, `:488`). The only code that ever sets the flag to `true` is `worker.rs:177`, inside the deleted worker. After deletion the flag is forever `false` and the two call sites become log-noise noops. This is Rust dead code, so it is removed here — not deferred. Only the **frontend** `speech-detected` listeners (3 components) are deferred; the indicator is already broken today (the Rust emitter dies with the worker), so this does not regress it.

### D6 — `get_transcript_history` Rust command deleted; frontend caller deferred

The command (`recording_commands.rs:1007`) returns `Vec<recording_saver::TranscriptSegment>` and is functionally dead (the segment list is always empty), but it is live API surface with a frontend caller (`transcriptService.ts:38` → `TranscriptContext.tsx:252`, used for reload-during-recording sync). The Rust command is deleted first (task 3.1, before the struct, per D1's retire-callers-first ordering); `transcriptService.getTranscriptHistory()` + the `TranscriptContext` reload-sync `useEffect` become dead adapter wiring tracked in the follow-up issue.

### D7 — Dead channel plumbing removed

The `transcription_sender`/`transcription_receiver` `mpsc::unbounded_channel::<AudioChunk>()` exists only to feed the deleted worker. Remove: the allocation in `recording_manager.rs:82`, the sender threaded through `AudioPipeline::new` (`pipeline.rs:707`, dropped at `:726`), and the `_transcription_receiver` bindings at `recording_commands.rs:311`/`:456`. `start_recording`'s signature loses the receiver it returns. Note: there is a `#[cfg(test)]` red test at `pipeline.rs:1010` (`pipeline_does_not_initialise_vad`) keyed to `audio-recording-quality` task 1.2 — update or leave with a cross-reference, do not silently break.

### D8 — Spec deltas correct the lie without weakening the mandate (unchanged)

`whisper-model-selection` MODIFIED: reframe the token requirement onto the live save paths, drop the `TranscriptUpdate` clause; storage mandate preserved. `recording-lifecycle` REMOVED+ADDED: drop the dead `TranscriptUpdate`-event requirement, re-add `TranscriptSegment carries an optional speaker field` preserving the `diarization-complete` scenario. `audio-recording-quality:178` (no `transcript-update` during recording) is already accurate — untouched.

### Hexagonal boundaries

All deletions are adapter-layer (`audio/`, `api/`, `pipeline.rs`, `recording_manager`). The domain (`common::TranscriptSegment`), ports, and use-cases are untouched. Subtractive only.

## Risks / Trade-offs

- **[Over-deletion into a live path]** Removing a symbol the queue/import/summary chain uses. → Mitigated by D1's pre-enumerated table + the §1 guards (meeting row, audio.mp4, summary reads `transcripts.json`, live token tests).
- **[Audio-save entanglement]** Breaking `stop_and_save` while removing the segment role. → Mitigated by an adversarial test asserting `audio.mp4` exists after stop.
- **[Meeting-row regression]** → D2 + guard.
- **[Auto-summary starvation]** Removing the wrong `transcripts.json` writer. → Mitigated by D4 (delete only the `recording_saver` writer; `common::` feeds the reader) + a guard asserting the summary chain reads `transcripts.json` after a retranscription flow.
- **[Speech-detected indicator stays broken]** Accepted; already broken today.
- **[Scope creep]** D5/D7 expand the change beyond the original "worker + saver struct." Accepted — both are backend Rust dead code orphaned by the worker deletion, consistent with "backend dead code" scope; leaving them would contradict the proposal's own rationale.

## Adversarial tests (§4)

- **Stop flow persists the meeting row** — stop without transcription; `meetings` row exists (queue discoverability).
- **Audio file still written** — meeting folder contains `audio.mp4` after stop (saver audio role survives).
- **Auto-summary chain unbroken** — after a retranscription-only flow, `transcripts.json` exists and the summary processor reads it (the `common::` writer, not the deleted one, feeds the reader).
- **Live token path unbroken** — existing retranscription/import token-binding tests stay green.
- **Dead path fully gone** — `cargo build` green; grep finds no `start_transcription_task`, no `recording_saver::TranscriptSegment`, no `From<recording_saver::TranscriptSegment>`, no `recording_saver_old`, no `lib_old_complex`, no `reset_speech_detected_flag`, no `SPEECH_DETECTED_EMITTED`, no `transcription_sender`/`_transcription_receiver` plumbing.
- **No worker-emitted event remains** — grep confirms no live `emit(...)` for any of `transcript-update`, `speech-detected`, `transcription-error`, `transcription-progress`, `transcription-queue-complete`, `transcript-chunk-loss-detected` in compiled code.
