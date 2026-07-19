## 1. Apply-time re-verification (design D6)

- [x] 1.1 Fresh grep + file:line re-confirm for the FULL inventory: original sites plus shark-tank additions (`post_processor.rs`, `TranscriptView.tsx` zero-importer, streaming sites, `Transcript` dead-field readers at `TranscriptContext.tsx:131-330`). If any check fails (code shifted, new caller appeared), pause and re-panel that specific claim before proceeding.

## 2. Dead listener removal + cascading prop/state

- [x] 2.1 Baseline: `pnpm test:smoke` green BEFORE any edit.
- [x] 2.2 Remove `speech-detected` listener from `RecordingControls.tsx` (:258) and `onSpeechDetected` from `recordingService.ts` (:257). (The `TranscriptView.tsx` listener dies with the file in §6.)
- [x] 2.3 Remove `transcript-error` listener (`RecordingControls.tsx:199`) and `onTranscriptError` wrapper (`transcriptService.ts:76`).
- [x] 2.4 Remove `transcription-error` listener (`RecordingControls.tsx:220`, `useModalState.ts:135`) and `onTranscriptionError` wrapper (`:66`). Verify `model-loading-failed` (live) and `retranscription-error` (live) are NOT touched.
- [x] 2.5 Remove `transcription-complete` listener and `onTranscriptionComplete` wrapper (`:57`). (Test replacement in §8.)
- [x] 2.6 Remove the `onTranscriptionError` **prop chain**: interface (`RecordingControls.tsx:19`), destructure (`:34`), `useEffect` deps array (`:284`), and the JSX pass-through at `app/page.tsx:257`.
- [x] 2.7 Remove the write-only `transcriptionErrors` state (`RecordingControls.tsx:50`) — its only setters were the listeners removed in 2.3/2.4.
- [x] 2.8 Verify: `pnpm test` + `pnpm test:smoke` green; `tsc --noEmit` clean.

## 3. TranscriptContext surgery + recording-started listener test (design D5)

- [x] 3.1 **Write first (red)**: Vitest test proving the kept `recording-started` listener (`TranscriptContext.tsx:88-129`) updates `currentMeetingId`/`activeMeetingId`/`meetingTitle`. Extract the listener body to a pure helper and import the real one, OR mount `TranscriptProvider` and emit `recording-started` via the mock bus (per [[hook-testing-extract-pure-helpers]]). This closes the biggest test gap — the smoke suite stays green today even if surgery nicks this listener.
- [x] 3.2 Created `frontend/e2e/smoke/cleanup-realtime-transcription-orphans.spec.ts`. **Deviation from original text:** the `page.evaluate` flip of `get_recording_state` → `phase: 'Recording'` did not flip `isRecording` post-reload (a standalone emit of `recording-state-changed` after reload is silently inert, while the identical emit inside the `start_recording` mock handler works — proven path in `recording-basic.spec.ts`). Pivoted to driving start via the sidebar Mic click (real user path), which emits both events from inside the mock handler. Spec asserts: idle copy renders pre- AND post-reload (reload re-runs the mount effect that used to call dead `get_transcript_history`), Mic-click starts recording, stop button mounts, no `pageerror`. Strictly more coverage than the original plan (also exercises start-command dispatch + `recording-started` metadata listener). design.md §4 amended to match.
- [x] 3.3 Remove buffer machinery from `contexts/TranscriptContext.tsx`: `transcriptBuffer`, `processBufferedTranscripts`, `sortTranscripts`, the buffer `useEffect` (:132-240), `flushBuffer` (:357), `finalFlushRef` (:42), AND the `flushBuffer` entries in the context type (:16) and value (:398).
- [x] 3.4 Remove `addTranscript` (:289), its context-value exposure (:396), and its type in `TranscriptContextType` (:14).
- [x] 3.5 Remove `syncFromBackend` reload-sync effect (:244-286) and its caller.
- [x] 3.6 Remove now-stale imports: `transcriptService` (:7) and the `TranscriptUpdate`/`TranscriptHistorySegment` symbols from the `:4` import.
- [x] 3.7 Verified: `pnpm test:smoke` 31 passed (30 baseline + the new 3.2 spec); `pnpm test` 261 green (12.2); `tsc --noEmit` clean (12.2).

## 4. Delete transcriptService.ts (design D2)

- [x] 4.1 Delete `frontend/src/services/transcriptService.ts` entirely (the `TranscriptionStatus` TS interface dies with it).
- [x] 4.2 Grep-confirm zero remaining imports of `transcriptService` / `TranscriptService` in `frontend/src`.
- [x] 4.3 Verify: `tsc --noEmit` clean.

## 5. Delete dead types + Transcript dead fields (design D9)

- [x] 5.1 Remove the stale `import { TranscriptUpdate }` at `services/indexDBService.ts:11`.
- [x] 5.2 Delete `TranscriptUpdate` (:34) and `TranscriptHistorySegment` (:8) from `types/index.ts`.
- [x] 5.3 Remove the three dead optional fields from the live `Transcript` type: `sequence_id?`, `chunk_start_time?`, `is_partial?` (:23-25). (Every reader is in the dead code removed in §3.)
- [x] 5.4 Verify: `tsc --noEmit` clean; grep for both type names + the three fields returns zero hits in `frontend/src`.

## 6. Delete TranscriptView.tsx entirely (design D7)

- [x] 6.1 Delete `frontend/src/components/TranscriptView.tsx` (zero importers — takes `speechDetected`, `SpeechDetectedEvent`, streaming block, and stale copy with it).
- [x] 6.2 Grep-confirm zero remaining references (incl. `frontend/e2e` — only the stale comment in `prompt-injection.spec.ts:18`, fixed in 8.6).
- [x] 6.3 Verify: `tsc --noEmit` clean.

## 7. Streaming machinery removal (design D8)

- [x] 7.1 Delete `frontend/src/hooks/useTranscriptStreaming.ts` (entire file — no-op post-parent).
- [x] 7.2 Strip from `VirtualizedTranscriptView.tsx`: import (:6), `enableStreaming` prop (:26-27, :163), the `useTranscriptStreaming` call (:208-212), `isStreaming` usage (:339, :410); `getDisplayText(segment)` → `segment.text`.
- [x] 7.3 Remove the `enableStreaming={isRecording}` prop at `app/_components/TranscriptPanel.tsx:115`.
- [x] 7.4 Remove the `enableStreaming={false}` prop at `components/MeetingDetails/TranscriptPanel.tsx:212`.
- [x] 7.5 Verify: `pnpm test` + `pnpm test:smoke` green; `tsc --noEmit` clean.

## 8. Fixture migration + prompt-injection split (design D3, D4)

- [x] 8.1 Define a local `FixtureSegment` interface in `e2e/_fixtures/loader.ts` matching the existing 8 fixture fields; replace the import (:1), the `segments` field type (:9), and `satisfies TranscriptHistorySegment` (:87).
- [x] 8.2 Verify `e2e/_fixtures/loader.test.ts` 18 cases pass unchanged.
- [x] 8.3 Vitest injection test on `computeDisplayText` (the exact function `TranscriptSegment` calls). **Deviation from original text:** `vitest.config.ts` has no `@vitejs/plugin-react`, so JSX compiles to the classic `React.createElement` runtime and React components can't render in tests (no existing test renders JSX — all follow [[hook-testing-extract-pure-helpers]]). Adding the plugin is test-infra scope-creep. Pivoted to the project's pure-function pattern: extracted `computeDisplayText` (`VirtualizedTranscriptView.tsx`) and test it with §4 adversarial payloads proving pass-through (no HTML sanitization — escaping is React's job, guarded separately by 8.4). 11 tests in `src/__tests__/transcript-segment-injection.test.ts`.
- [x] 8.4 `dangerouslySetInnerHTML` guard in `src/__tests__/dangerously-set-inner-html-guard.test.ts`: walks `frontend/src` (excl. `__tests__`, `.test.*`), skips comment lines, asserts the only offender file is `app/notes/[id]/page.tsx` (trusted markdown baseline). Any new usage in a transcript-rendering component fails the suite.
- [x] 8.5 Re-anchor prompt-injection Test A to `recording-started`; drop the `get_transcript_history` mock registration (`prompt-injection.spec.ts:63`).
- [x] 8.6 Delete Test B from the Playwright harness (now covered by 8.3); fix the stale "the app's TranscriptView component" comment (:18).
- [x] 8.7 Verify: `pnpm test` (261) + `pnpm test:smoke` (30) green; prompt-injection Playwright spec passes (Test A re-anchored).

## 9. Rust corpse cleanup + post_processor.rs + IPC-absence test (design D10)

- [x] 9.1 **Write first (red)**: Rust unit test `get_transcription_status_absent_from_invoke_handler_table` in `lib.rs` test module — uses `include_str!("lib.rs")` + `find("generate_handler![")` / `find("])")` to extract the handler block and assert the command is absent. RED before deletion, GREEN after. Closes the macro-registration loophole (proves no runtime dispatch, not just no source reference).
- [x] 9.2 Deleted the registered `get_transcription_status` stub, its invoke-handler registration, and its private `TranscriptionStatus` struct from `lib.rs`.
- [x] 9.3 Deleted the unregistered `get_transcription_status` impl + `pub struct TranscriptionStatus` from `audio/recording_commands.rs`, and the `get_transcription_status, TranscriptionStatus` entries from the `audio/mod.rs` re-export.
- [x] 9.4 Deleted `audio/post_processor.rs` (entire file), its `audio/mod.rs` module declaration, and its `pub use post_processor::{...}` re-export.
- [x] 9.5 Verify: `cargo test --lib` 486 passed / 0 failed / 16 ignored; grep confirms `get_transcription_status` only in the guard test (3 hits, all in the test module), `PostProcessor`/`post_processor` zero hits in `frontend/src-tauri/src`.

## 10. Stale copy fix (the live site)

- [x] 10.1 Updated `VirtualizedTranscriptView.tsx:308` "Start recording to see live transcription" → "Start recording — transcript generates after you stop." (matches the batch-pipeline tone of `:302`).

## 11. Cheap ride-alongs

- [x] 11.1 Swapped the dead `transcript-update` example → `recording-started` in `e2e/harness/event-bus.spec.ts` (comment + listen/emit/assertion) and `e2e/mocks/tauri-event-mock.ts` (comment). Payload reshaped to `{ meeting_id, title }`. Event-bus spec 2/2 green.
- [x] 11.2 Fixed stale "real-time" prose: `README.md:86` ("transcribes them in real-time" → "after each meeting"), `README.md:115` ("Real-time Transcription" feature → "Post-meeting Transcription"), `frontend/README.md:8` ("Live transcription" → "Batch transcription ... generated after each meeting ends"). Left `frontend/README.md:7` "real-time audio recording" (recording IS real-time; only transcription is batch).

## 12. Spec sync + merge gate (pre-archive)

- [x] 12.1 Synced both deltas into canonical specs. `audio-recording-quality`: generalized "`transcript-update` events" → "transcript-content Tauri events" + added "transcription runs strictly post-meeting via the retranscription queue or the import path" (requirement body + Pipeline-skips-VAD scenario). `whisper-model-selection`: replaced "No `TranscriptUpdate` Tauri event is emitted" → "No transcript content is delivered to the UI during recording via Tauri events" (removes the deleted-type reference). Both deltas were MODIFIED requirements; bodies + scenarios now match the delta verbatim.
- [x] 12.2 Full §7 merge gate: `cargo test` (486 lib + 2 integration, 0 failed, 17 ignored), `pytest backend/` (6 passed), `pnpm test` (261 passed), `pnpm lint` (warnings only — all pre-existing), `pnpm test:smoke` (32 passed — 30 baseline + the 3.2 reload test + the 12.3 record→stop→view test). All five gates green.
- [~] 12.3 **Partially verified — wiring only, not the real pipeline.** Added a second test case to `e2e/smoke/cleanup-realtime-transcription-orphans.spec.ts` that drives record→stop→`/meeting-details?id=<id>` with the mock backend and asserts (b) transcript segment renders, (c) summary block renders, (d) no `pageerror`. (a) sidebar row is covered by `recording-basic.spec.ts` 4.1. **Honest gap:** the transcript text and summary come from mock fixtures, NOT from real Whisper or a real LLM — so "summary generates" means "the mock serves a summary and the UI renders it," not "the LLM produced a summary from real audio." Real mic + WASAPI capture never runs. No automated test in the repo covers the full record→stop→transcribe→summarize UI flow with real models: the `#[ignore]` tests (`retranscription.rs:1929`, `vad.rs:598`, `recording_manager.rs:642`, speaker/diarization) each exercise a subsystem with real audio files, not the end-to-end UI flow. **Why this is acceptable for THIS change:** the cleanup doesn't touch capture, Whisper, or the LLM (design Non-Goals: "No change to post-meeting transcript rendering behavior, diarization, or storage"), so the real pipeline is byte-for-byte unchanged — if it worked before, it works now. The wiring test covers the surface this cleanup actually risks (frontend listeners, TranscriptContext surgery, streaming removal). A live-mic run would verify code this change didn't alter. Deferred to user's discretion.
- [x] 12.4 Shark-tank adversarial code review CONVERGED. Three parallel forks (deletion-safety, test-quality+regression, spec/design+scope) agreed on two blockers: Task 3.2 smoke spec (this session: created + green, 31/31 smoke) and design.md D4 amendment (prior session: documented the computeDisplayText pivot; this session: added the 3.2 Mic-click pivot rationale). All other findings MINOR and accepted: helper-test-not-integration (per [[hook-testing-extract-pure-helpers]]), vestigial `transcripts` state (pre-existing, not a regression — syncFromBackend was already silently broken calling deleted `get_transcript_history`), XSS coverage indirect but justified. Fork 1 (deletion safety) ARCHIVE-READY with zero findings across all 10 falsification categories.
