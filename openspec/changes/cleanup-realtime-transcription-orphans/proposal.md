## Why

The merged `remove-dead-realtime-transcription` change (archived 2026-07-17) deleted the unsupported realtime-during-recording transcription Rust path but explicitly **deferred all orphan cleanup** to this follow-up (`openspec/changes/archive/2026-07-17-remove-dead-realtime-transcription/design.md:43`). A 4-panelist shark-tank review of this proposal's first draft (2026-07-18) caught a whole dead Rust file, a whole dead component, a copy fix aimed at the wrong file, and cascading orphans; its findings are folded in below. No user-facing capability is lost — every dead piece fed the live-during-recording transcript display that was intentionally removed.

## What Changes

### Dead event listeners (no Rust emitter)
Remove listeners for `speech-detected`, `transcript-error`, `transcription-error`, `transcription-complete` (verified zero emitters across the full Rust `emit` inventory + backend). Sites: `RecordingControls.tsx`, `hooks/useModalState.ts`, `services/recordingService.ts`, `services/transcriptService.ts`.

### Dead service singleton
Delete `services/transcriptService.ts` entirely — all 7 methods dead. The `model-download-complete` / `parakeet-model-download-complete` *events* stay live (emitters in whisper/parakeet/ollama, 4+ direct listeners each); only the unused wrapper methods die. The `TranscriptionStatus` TS interface inside the file dies with it.

### `TranscriptContext` surgery
Remove the buffer machinery (`transcriptBuffer`, `processBufferedTranscripts`, `sortTranscripts`, `flushBuffer`, `finalFlushRef`), `addTranscript`, the reload-sync `syncFromBackend` effect (calls the deleted `get_transcript_history` command — silently broken), and the now-stale `transcriptService` import (`:7`) + `TranscriptUpdate`/`TranscriptHistorySegment` symbols (`:4`). Keep `transcripts` state, recording-metadata listeners, `copyTranscript`, `clearTranscripts`, `markMeetingAsSaved`. (Verified the four effects are structurally separable; the metadata listener shares no scope with the buffer effect.)

### Cascading orphans from listener removal
- `RecordingControls.tsx`: the `onTranscriptionError` **prop** (`:19`, `:34`, deps `:284`) and the write-only `transcriptionErrors` state (`:50`) become dead once the listeners at `:199`/`:220` are gone.
- `app/page.tsx:257`: the `onTranscriptionError` JSX pass-through (`showModal('errorAlert', message)`).

### Dead component — `TranscriptView.tsx`
Delete the entire file. Zero importers in `frontend` (both live `TranscriptPanel`s render `VirtualizedTranscriptView`). Its `speechDetected` state, `SpeechDetectedEvent` interface, streaming-typewriter block, and stale empty-state copy all die with the file.

### Dead types
- Delete `TranscriptUpdate` and `TranscriptHistorySegment` from `types/index.ts`.
- Remove the stale `TranscriptUpdate` import at `services/indexedDBService.ts:11` (latent build break — `tsconfig` lacks `noUnusedLocals`).
- Remove three dead optional fields from the live `Transcript` type (`sequence_id?`, `chunk_start_time?`, `is_partial?`) — every reader is inside the dead buffer/sync/addTranscript code being removed.

### Streaming typewriter machinery (now a no-op)
Delete `hooks/useTranscriptStreaming.ts` (whole file — early-returns when no segments arrive, which is always). Strip the `enableStreaming` prop, the `useTranscriptStreaming` call, and `isStreaming`/`getDisplayText` usage from `VirtualizedTranscriptView.tsx` (`getDisplayText(segment)` → `segment.text`). Drop the `enableStreaming` prop from both `TranscriptPanel`s (`app/_components/TranscriptPanel.tsx:115` was `{isRecording}`; `MeetingDetails/TranscriptPanel.tsx:212` was `false`).

### Fixture migration
Migrate `e2e/_fixtures/loader.ts` off `TranscriptHistorySegment` to a local self-describing `FixtureSegment` (the live `TranscriptSegmentData` shape is incompatible; fixtures should not mirror production types). The 18-case `loader.test.ts` never names the type — asserts on structure — so it passes unchanged.

### Prompt-injection coverage (§4) — split
`e2e/harness/prompt-injection.spec.ts` drives both tests via the dead `transcription-complete` event. Test A (dispatcher integrity — re-anchored to the thinner `recording-started` event) keeps the property that adversarial payloads cannot register/redirect commands. Test B (transcript text renders inert) is replaced by a Vitest structural test that renders adversarial text through the real `TranscriptSegment` memo (`VirtualizedTranscriptView.tsx:73-155`, the actual `<p>{displayText}</p>` emitter — no virtualizer/Framer dependency) and asserts `innerHTML` inertness, **plus a repo-wide recursive grep** asserting the only `dangerouslySetInnerHTML` in `frontend/src` is the pre-existing `app/notes/[id]/page.tsx:174` (legitimate markdown rendering for the notes page — baseline); any additional hit fails the guard. Today's Test B doesn't render through React at all (manual `textContent` assignment), so the structural test is strictly stronger. Drop the `get_transcript_history` mock (`:63`).

### Rust corpses
- Registered hardcoded-zero stub `get_transcription_status` (`lib.rs:313`, registered `:1066`) + its private `TranscriptionStatus` struct (`lib.rs:78`).
- Unregistered phase-reading impl (`audio/recording_commands.rs:860`, struct `:110`) + re-export (`audio/mod.rs:92`).
- **`audio/post_processor.rs` — entire file** (missed by the first draft; caught by shark-tank). Zero external callers; the `PostProcessor`/`PostProcessRequest`/`PostProcessResponse` symbols appear only inside the file and its `audio/mod.rs:30,100` module decl + re-export. It was the sink for the deleted `start_transcription_task` feeder.

### Stale copy fix (the actual user-visible regression)
`VirtualizedTranscriptView.tsx:323` — the live idle-state copy "Start recording to see live transcription" promises realtime transcription that no longer exists. (Lines `:314`/`:317` already use post-meeting phrasing; only `:323` is stale.) Replace with copy matching that tone, e.g. "Start recording — transcript generates after you stop."

### Cheap ride-alongs
- `e2e/harness/event-bus.spec.ts:27,30,35` + `e2e/mocks/tauri-event-mock.ts:4` — swap the dead `transcript-update` example for a live event.
- `README.md:86,115` + `frontend/README.md:8` — stale "real-time transcription" prose contradicting the fork's batch-pipeline description.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `whisper-model-selection`: the requirement forbidding realtime transcription (`spec.md:36`) references the `TranscriptUpdate` type by name, which this change deletes. Rephrased to forbid the behavior without naming a type that no longer exists.
- `audio-recording-quality`: the "no VAD or Whisper during capture" requirement (`spec.md:178,185`) names the dead `transcript-update` event. Rephrased to forbid transcript-content emission during recording without naming the dead event.

## Impact

- **Frontend source**: `services/transcriptService.ts` (delete), `services/recordingService.ts`, `services/indexedDBService.ts`, `contexts/TranscriptContext.tsx`, `components/RecordingControls.tsx`, `components/VirtualizedTranscriptView.tsx`, `components/TranscriptView.tsx` (delete), `hooks/useTranscriptStreaming.ts` (delete), `hooks/useModalState.ts`, `app/page.tsx`, `app/_components/TranscriptPanel.tsx`, `components/MeetingDetails/TranscriptPanel.tsx`, `types/index.ts`.
- **Fixtures / tests**: `e2e/_fixtures/loader.ts`, `e2e/harness/prompt-injection.spec.ts`, `e2e/harness/event-bus.spec.ts`, `e2e/mocks/tauri-event-mock.ts`, new `e2e/smoke/cleanup-realtime-transcription-orphans.spec.ts`, new Vitest structural injection test + new Vitest test for the kept `recording-started` listener.
- **Rust**: `src-tauri/src/lib.rs`, `src-tauri/src/audio/recording_commands.rs`, `src-tauri/src/audio/mod.rs`, `src-tauri/src/audio/post_processor.rs` (delete).
- **Specs**: `whisper-model-selection`, `audio-recording-quality`.
- **Docs**: `README.md`, `frontend/README.md`.
- **No behavioral change** to the live record → stop → post-meeting-transcribe flow (smoke-verified 30/30 green today). No API, dependency, or storage changes.
- **Non-Goal** (filed separately): `audio/transcription/provider.rs:45` `TranscriptResult.is_partial` — dead data on a live Rust trait type; trait-level blast radius is bigger than this change. The `model-loading-failed` event has zero frontend listeners (pre-existing gap) — also out of scope.
- **Gates**: `tsc --noEmit`, `cargo test`, `pnpm test`, `pnpm test:smoke`, a Rust unit test asserting `get_transcription_status` is absent from the invoke-handler table, and a live record→stop→transcribe manual check (smoke mocks the backend).
