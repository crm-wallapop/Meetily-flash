## 1. Apply-time re-verification (design D6)

- [ ] 1.1 Fresh grep + file:line re-confirm for the FULL inventory: original sites plus shark-tank additions (`post_processor.rs`, `TranscriptView.tsx` zero-importer, streaming sites, `Transcript` dead-field readers at `TranscriptContext.tsx:131-330`). If any check fails (code shifted, new caller appeared), pause and re-panel that specific claim before proceeding.

## 2. Dead listener removal + cascading prop/state

- [ ] 2.1 Baseline: `pnpm test:smoke` green BEFORE any edit.
- [ ] 2.2 Remove `speech-detected` listener from `RecordingControls.tsx` (:258) and `onSpeechDetected` from `recordingService.ts` (:257). (The `TranscriptView.tsx` listener dies with the file in §6.)
- [ ] 2.3 Remove `transcript-error` listener (`RecordingControls.tsx:199`) and `onTranscriptError` wrapper (`transcriptService.ts:76`).
- [ ] 2.4 Remove `transcription-error` listener (`RecordingControls.tsx:220`, `useModalState.ts:135`) and `onTranscriptionError` wrapper (`:66`). Verify `model-loading-failed` (live) and `retranscription-error` (live) are NOT touched.
- [ ] 2.5 Remove `transcription-complete` listener and `onTranscriptionComplete` wrapper (`:57`). (Test replacement in §8.)
- [ ] 2.6 Remove the `onTranscriptionError` **prop chain**: interface (`RecordingControls.tsx:19`), destructure (`:34`), `useEffect` deps array (`:284`), and the JSX pass-through at `app/page.tsx:257`.
- [ ] 2.7 Remove the write-only `transcriptionErrors` state (`RecordingControls.tsx:50`) — its only setters were the listeners removed in 2.3/2.4.
- [ ] 2.8 Verify: `pnpm test` + `pnpm test:smoke` green; `tsc --noEmit` clean.

## 3. TranscriptContext surgery + recording-started listener test (design D5)

- [ ] 3.1 **Write first (red)**: Vitest test proving the kept `recording-started` listener (`TranscriptContext.tsx:88-129`) updates `currentMeetingId`/`activeMeetingId`/`meetingTitle`. Extract the listener body to a pure helper and import the real one, OR mount `TranscriptProvider` and emit `recording-started` via the mock bus (per [[hook-testing-extract-pure-helpers]]). This closes the biggest test gap — the smoke suite stays green today even if surgery nicks this listener.
- [ ] 3.2 Create `frontend/e2e/smoke/cleanup-realtime-transcription-orphans.spec.ts`: capture `pageerror` (assert empty); positive signal (corrected idle copy from task 10.1 renders); `page.evaluate` post-reload to flip mock `get_recording_state` → `phase: 'Recording'`, then assert the status bar renders and no crash. (The harness resets `__smokeRecording` on every init — without the mock flip the spec is a tautology.)
- [ ] 3.3 Remove buffer machinery from `contexts/TranscriptContext.tsx`: `transcriptBuffer`, `processBufferedTranscripts`, `sortTranscripts`, the buffer `useEffect` (:132-240), `flushBuffer` (:357), `finalFlushRef` (:42), AND the `flushBuffer` entries in the context type (:16) and value (:398).
- [ ] 3.4 Remove `addTranscript` (:289), its context-value exposure (:396), and its type in `TranscriptContextType` (:14).
- [ ] 3.5 Remove `syncFromBackend` reload-sync effect (:244-286) and its caller.
- [ ] 3.6 Remove now-stale imports: `transcriptService` (:7) and the `TranscriptUpdate`/`TranscriptHistorySegment` symbols from the `:4` import.
- [ ] 3.7 Verify: `pnpm test` (incl. 3.1) + `pnpm test:smoke` (incl. 3.2) green; `tsc --noEmit` clean.

## 4. Delete transcriptService.ts (design D2)

- [ ] 4.1 Delete `frontend/src/services/transcriptService.ts` entirely (the `TranscriptionStatus` TS interface dies with it).
- [ ] 4.2 Grep-confirm zero remaining imports of `transcriptService` / `TranscriptService` in `frontend/src`.
- [ ] 4.3 Verify: `tsc --noEmit` clean.

## 5. Delete dead types + Transcript dead fields (design D9)

- [ ] 5.1 Remove the stale `import { TranscriptUpdate }` at `services/indexDBService.ts:11`.
- [ ] 5.2 Delete `TranscriptUpdate` (:34) and `TranscriptHistorySegment` (:8) from `types/index.ts`.
- [ ] 5.3 Remove the three dead optional fields from the live `Transcript` type: `sequence_id?`, `chunk_start_time?`, `is_partial?` (:23-25). (Every reader is in the dead code removed in §3.)
- [ ] 5.4 Verify: `tsc --noEmit` clean; grep for both type names + the three fields returns zero hits in `frontend/src`.

## 6. Delete TranscriptView.tsx entirely (design D7)

- [ ] 6.1 Delete `frontend/src/components/TranscriptView.tsx` (zero importers — takes `speechDetected`, `SpeechDetectedEvent`, streaming block, and stale copy with it).
- [ ] 6.2 Grep-confirm zero remaining references (incl. `frontend/e2e` — only the stale comment in `prompt-injection.spec.ts:18`, fixed in 8.6).
- [ ] 6.3 Verify: `tsc --noEmit` clean.

## 7. Streaming machinery removal (design D8)

- [ ] 7.1 Delete `frontend/src/hooks/useTranscriptStreaming.ts` (entire file — no-op post-parent).
- [ ] 7.2 Strip from `VirtualizedTranscriptView.tsx`: import (:6), `enableStreaming` prop (:26-27, :163), the `useTranscriptStreaming` call (:208-212), `isStreaming` usage (:339, :410); `getDisplayText(segment)` → `segment.text`.
- [ ] 7.3 Remove the `enableStreaming={isRecording}` prop at `app/_components/TranscriptPanel.tsx:115`.
- [ ] 7.4 Remove the `enableStreaming={false}` prop at `components/MeetingDetails/TranscriptPanel.tsx:212`.
- [ ] 7.5 Verify: `pnpm test` + `pnpm test:smoke` green; `tsc --noEmit` clean.

## 8. Fixture migration + prompt-injection split (design D3, D4)

- [ ] 8.1 Define a local `FixtureSegment` interface in `e2e/_fixtures/loader.ts` matching the existing 8 fixture fields; replace the import (:1), the `segments` field type (:9), and `satisfies TranscriptHistorySegment` (:87).
- [ ] 8.2 Verify `e2e/_fixtures/loader.test.ts` 18 cases pass unchanged.
- [ ] 8.3 Add a Vitest structural injection test: render the real `TranscriptSegment` memo (`VirtualizedTranscriptView.tsx:73-155`) with adversarial payloads (`<script>`, `<img onerror>`, `javascript:` URL, template injection, SVG) in jsdom; assert `innerHTML` contains only literal text.
- [ ] 8.4 Add a `dangerouslySetInnerHTML` guard over `frontend/src/**/*.{tsx,ts}`: assert the only hit is the pre-existing `app/notes/[id]/page.tsx:174` (legitimate markdown rendering for notes — baseline). Any additional hit (e.g., someone adds it to a transcript-rendering component) fails the guard.
- [ ] 8.5 Re-anchor prompt-injection Test A to `recording-started`; drop the `get_transcript_history` mock registration (`prompt-injection.spec.ts:63`).
- [ ] 8.6 Delete Test B from the Playwright harness (now covered by 8.3); fix the stale "the app's TranscriptView component" comment (:18).
- [ ] 8.7 Verify: `pnpm test` + `pnpm test:smoke` green.

## 9. Rust corpse cleanup + post_processor.rs + IPC-absence test (design D10)

- [ ] 9.1 **Write first (red)**: Rust unit test asserting `get_transcription_status` is absent from the invoke-handler table (closes the macro-registration loophole — proves "no runtime dispatch," not just "no source reference").
- [ ] 9.2 Delete the registered `get_transcription_status` stub (`lib.rs:313`), its invoke-handler registration (`lib.rs:1066`), and its private `TranscriptionStatus` struct (`lib.rs:78`).
- [ ] 9.3 Delete the unregistered `get_transcription_status` impl (`audio/recording_commands.rs:860`, struct `:110`) and its re-export (`audio/mod.rs:92`).
- [ ] 9.4 Delete `audio/post_processor.rs` (entire file — zero external callers) and its `audio/mod.rs` module declaration (`:30`) + re-export (`:100`).
- [ ] 9.5 Verify: `cargo test` + `cargo check` clean; grep `get_transcription_status` + `PostProcessor` returns zero hits in `frontend/src-tauri/src`.

## 10. Stale copy fix (the live site)

- [ ] 10.1 Update `VirtualizedTranscriptView.tsx:323` "Start recording to see live transcription" → post-meeting copy matching the tone of `:317` (e.g., "Start recording — transcript generates after you stop."). Verify `:314`/`:317` remain consistent.

## 11. Cheap ride-alongs

- [ ] 11.1 Swap the dead `transcript-update` example in `e2e/harness/event-bus.spec.ts:27,30,35` and `e2e/mocks/tauri-event-mock.ts:4` → a live event (`recording-started`).
- [ ] 11.2 Fix stale "real-time transcription" prose in `README.md:86,115` and `frontend/README.md:8` to match the fork's batch-pipeline description.

## 12. Spec sync + merge gate (pre-archive)

- [ ] 12.1 Sync both deltas (`whisper-model-selection`, `audio-recording-quality`) into canonical specs at archive time (re-read each canonical spec + delta before `/opsx:archive`).
- [ ] 12.2 Full §7 merge gate in parallel: `cargo test`, `pytest backend/`, `pnpm test`, `pnpm lint`, `pnpm test:smoke` — all green.
- [ ] 12.3 Live manual check (concrete): default mic + WASAPI loopback; record ≥30s with ≥10s clear speech; assert (a) meeting row appears in sidebar with correct title, (b) `/meeting-details?id=<id>` renders transcript segments, (c) summary generates, (d) no console errors during the flow.
- [ ] 12.4 Shark-tank adversarial code review of the implementation to convergence before archive.
