## Context

The archived `remove-dead-realtime-transcription` change deleted the Rust realtime-during-recording worker (`start_transcription_task`) and the transcription sender in the audio pipeline. Its `design.md:43` explicitly deferred all orphan cleanup to a follow-up. A 4-panelist shark-tank review of this change's first draft (2026-07-18, lenses: falsification, scope-completeness, test-adequacy, risk/blast-radius) caught a whole dead Rust file (`post_processor.rs`), a whole dead component (`TranscriptView.tsx`), a copy fix aimed at the wrong file, cascading orphans, and several test gaps. The falsification lens independently CONVERGED: every "dead" claim holds against the current tree. Those findings are folded in below.

The dead surface, verified first-hand:
- **Dead event listeners** for `speech-detected`, `transcript-error`, `transcription-error`, `transcription-complete` — zero Rust emitters.
- **`transcriptService.ts`** — 7 dead methods; only `getTranscriptHistory()` had a caller, and that caller is the dead `syncFromBackend` effect.
- **`TranscriptContext` buffer machinery** + `addTranscript` + `syncFromBackend` — no feeder.
- **`TranscriptView.tsx`** — zero importers; both live panels render `VirtualizedTranscriptView`.
- **`useTranscriptStreaming.ts`** + streaming props — no-op (early-returns when no segments arrive, which is always post-parent).
- **Dead types** `TranscriptUpdate`, `TranscriptHistorySegment`, and three dead optional fields on the live `Transcript` type (`sequence_id?`, `chunk_start_time?`, `is_partial?`) — every reader is inside the dead buffer/sync/addTranscript code.
- **Rust corpses**: `get_transcription_status` stub + dup struct + unregistered impl + re-export, and the entire `post_processor.rs` sink.
- **Stale copy** at `VirtualizedTranscriptView.tsx:323` (the live idle state).

Per the [[deletion-claims-need-falsification]] rule, each deletion is re-confirmed at apply time (D6).

## Goals / Non-Goals

**Goals:**
- Remove every orphan the parent change left behind, so the tree contains no reference to the removed realtime path — frontend, Rust, fixtures, specs, and docs.
- Fix the one user-visible regression (stale idle-state copy at the live site) and the one silent failure (reload-sync).
- Replace the §4 prompt-injection coverage with a `computeDisplayText` pass-through test + a repo-wide `dangerouslySetInnerHTML` guard (see D4 amendment for the honest trade vs. the deleted Test B), and close the testing gap on the kept `recording-started` listener.
- Leave the live record → stop → post-meeting-transcribe flow byte-for-byte unchanged.

**Non-Goals:**
- `audio/transcription/provider.rs:45` `TranscriptResult.is_partial` — dead data on a live Rust trait; trait-level blast radius exceeds this change. Filed separately.
- The `model-loading-failed` event has zero frontend listeners (pre-existing gap surfaced by the review). Out of scope; file separately if needed.
- No change to post-meeting transcript rendering behavior, diarization, or storage.

## Decisions

### D1. One bundled change
The parent's `design.md:43` framed this as a single follow-up. The fixture migration, type deletions, and `TranscriptView.tsx` deletion are all coupled. Bundling keeps the audit trail clean.

### D2. Delete `transcriptService.ts` entirely
All 7 methods dead. The `model-download-complete` / `parakeet-model-download-complete` events stay live; only the unused wrappers die. The `TranscriptionStatus` TS interface inside the file dies with it.

### D3. Fixture migrates to a local self-describing type
Shapes are incompatible (`TranscriptHistorySegment` vs live `TranscriptSegmentData`). A local `FixtureSegment` matching the fixture JSON's 8 fields decouples test data from production-type churn. The 18-case `loader.test.ts` asserts on structure, never the type name — passes unchanged.

### D4. Prompt-injection split — `computeDisplayText` pass-through test + `dangerouslySetInnerHTML` guard (AMENDED at apply time)

**Original plan (pre-apply):** render adversarial text through the real `TranscriptSegment` memo. **Apply-time discovery:** `vitest.config.ts` has no `@vitejs/plugin-react`, so esbuild compiles JSX to the classic `React.createElement` runtime and React components cannot render in Vitest (`ReferenceError: React is not defined` inside the component body). No existing test renders JSX — all follow the [[hook-testing-extract-pure-helpers]] pure-function pattern. Adding the plugin is test-infra scope-creep that re-costs every existing test's transform, so the design pivots to the project's established pattern.

**Pivot:** extract `computeDisplayText` (the exact function `TranscriptSegment` calls — `cleanStopWords(text) || (text.trim() === '' ? '[Silence]' : text)`) as a pure exported function, and assert it passes §4 adversarial payloads (`<script>`, `<img onerror>`, `javascript:` URL, template injection, SVG, SQL injection) through UNCHANGED — proving the text-processing layer never sanitizes or restructures markup. XSS protection is React's job (text content is escaped by default), and a **repo-wide `dangerouslySetInnerHTML` guard** (task 8.4, `src/__tests__/dangerously-set-inner-html-guard.test.ts`) walks `frontend/src/**/*.{tsx,ts}` (excl. `__tests__`, `.test.*`, comment lines) and asserts the only sanctioned bypass is the pre-existing `app/notes/[id]/page.tsx` notes-markdown baseline. Any new usage in a transcript-rendering component fails the suite.

**Honest trade vs. the deleted Test B (NOT "strictly stronger"):** the pivot loses Test B's live-DOM `innerHTML` assertion — but that assertion exercised a browser primitive (`textContent`) on the deleted `transcription-complete` listener, not our component. It gains (a) direct unit coverage of our actual text-processing function (which Test B never touched) and (b) a forward-looking repo-wide guard that Test B lacked. The DOM-inertness property still holds via React's well-established text escaping + the 8.4 guard.

- **Test A** (dispatcher integrity) re-anchors to `recording-started` (live, mock-emitted, listener only updates in-memory state — thinner than `recording-saved-to-db` which fires `markMeetingAsSaved` etc.).
- **Test B** → Vitest `computeDisplayText` pass-through test (11 cases in `transcript-segment-injection.test.ts`) + repo-wide `dangerouslySetInnerHTML` guard (`dangerously-set-inner-html-guard.test.ts`).

### D5. TranscriptContext surgery (verified separable)
The risk lens confirmed the four effects (recording-started listener `:88-129`, buffer machinery `:132-240`, `syncFromBackend` `:244-286`, autoscroll `:68-85`) are structurally separable — no shared refs except `transcriptsRef`/`transcripts` which stay. `setupRecordingListeners` does not share scope with the buffer effect. Remove only the dead parts; keep state + metadata listeners + `copyTranscript`/`clearTranscripts`/`markMeetingAsSaved`. Also remove the now-stale imports at `TranscriptContext.tsx:4,7`.

### D6. Apply-time re-verification per deletion
Before executing each deletion: re-confirm the file:line still matches and a fresh grep finds no new caller/emitter. The shark-tank verdict is from 2026-07-18; code may have shifted. If any re-check fails, pause and re-panel that claim.

### D7. Delete `TranscriptView.tsx` entirely (not surgical edit)
Zero importers. The first draft stripped its listener and fixed its copy — both pointless on a dead file. Delete the whole file; its `speechDetected` state, `SpeechDetectedEvent` interface, streaming block, and stale copy die with it.

### D8. Streaming machinery removal — contained live-component edit
`useTranscriptStreaming.ts` is a no-op (early-returns when `segments.length === 0`, always true post-parent). Delete the file. In `VirtualizedTranscriptView.tsx`: drop the import (`:6`), the `enableStreaming` prop (`:26-27`, `:163`), the hook call (`:208-212`), and the `isStreaming` usage (`:339`, `:410`); `getDisplayText(segment)` becomes `segment.text`. Drop the `enableStreaming` prop from both `TranscriptPanel`s. The edit is contained to the render-path of one live component; covered by existing `summary-render.spec.ts` + the new structural injection test (which renders `TranscriptSegment` directly).

### D9. Remove dead optional fields from the live `Transcript` type
`sequence_id?`, `chunk_start_time?`, `is_partial?` are only written by the dead `addTranscript`/`syncFromBackend` and only read inside the dead buffer/sort machinery. Every reader dies in §3. Remove the three fields from `Transcript` in the same task group that deletes `TranscriptUpdate`. `tsc --noEmit` catches any missed reader.

### D10. Delete `audio/post_processor.rs` (the orphaned sink)
Zero external callers — `PostProcessor`/`PostProcessRequest`/`PostProcessResponse` appear only inside the file and its `audio/mod.rs:30,100` module decl + re-export. It was the downstream text-refinement sink fed by the deleted `start_transcription_task`. Delete the file + the `mod.rs` decl (`:30`) and re-export (`:100`).

## Adversarial test plan (§4)

Deletion change; tests prove **non-regression**. The first draft's plan had gaps the test-adequacy lens caught; this is the corrected mapping.

| Category | Test |
|---|---|
| Transcription / Prompt injection | **NEW** (D4 as amended) Vitest `computeDisplayText` pass-through test: adversarial payloads through the real text-processing function `TranscriptSegment` calls; repo-wide `dangerouslySetInnerHTML` guard. Trades Test B's live-DOM assertion (tested a browser primitive on a deleted listener) for direct unit coverage of our text-processing + a forward-looking guard. |
| TranscriptContext / kept listener | **NEW** Vitest test (closes the biggest gap): the `recording-started` listener at `TranscriptContext.tsx:88-129` is currently untested — the smoke suite stays green even if surgery nicks it (the ref stale-guard skips when `expectedId` is null). Extract the listener body to a pure helper (per [[hook-testing-extract-pure-helpers]]) OR mount `TranscriptProvider` and emit `recording-started` via the mock bus; assert `currentMeetingId`, `activeMeetingId`, `meetingTitle` all update. |
| Reload-during-recording | **NEW** smoke `e2e/smoke/cleanup-realtime-transcription-orphans.spec.ts`: capture `pageerror` and assert empty; positive signal (corrected idle-state copy from D8-task renders) checked BOTH before and after a reload (the reload re-runs the mount effect that used to call the dead `get_transcript_history`); recording reconciliation post-reload driven by the sidebar Mic click (the real user path), asserting the stop button mounts. **Apply-time amendment:** the original plan (`page.evaluate` post-reload to flip `get_recording_state` → `phase: 'Recording'`) did not flip `isRecording` in the UI — a standalone emit of `recording-state-changed` after reload is silently inert, while the identical emit inside the `start_recording` mock handler works (proven path in `recording-basic.spec.ts`). Rather than chase the mock-lifecycle artifact, the spec drives start via the Mic click, which emits both `recording-started` and `recording-state-changed` from inside the mock handler. This is strictly more coverage (also exercises the start-command dispatch + the `recording-started` metadata listener the cleanup narrowed) and satisfies the non-tautology intent. |
| Transcription / Empty transcript | Existing `summary-render.spec.ts` empty-blocks case. Re-verify green. |
| Audio / Recording lifecycle | Existing `recording-basic.spec.ts`. Re-verify after listener + streaming removal. |
| Type/fixture continuity | Existing `loader.test.ts` (18 cases) passes after `FixtureSegment` migration. |
| Rust IPC absence | **NEW** Rust unit test: construct the invoke-handler table and assert `get_transcription_status` is absent. Turns "no source reference" into "no runtime dispatch" — closes the macro-registration loophole. |
| Build gates | `tsc --noEmit` (catches stale imports + Transcript field readers); `cargo test` + `cargo check`. |

## Risks / Trade-offs

- **[Stale import → hard build break]** Deleting `TranscriptUpdate` without removing the unused import at `indexedDBService.ts:11` (and the `:4` symbols in `TranscriptContext.tsx`) breaks `tsc`. Latent today (`tsconfig` lacks `noUnusedLocals`). → Mitigation: same task removes both; `tsc --noEmit` gate.

- **[TranscriptContext surgery nicks the kept listener]** The metadata listener shares the file but not scope with the dead buffer. → Mitigation: D5 verified separability; the new recording-started Vitest test (§4) directly covers the kept listener — previously untested.

- **[Streaming removal touches live `VirtualizedTranscriptView`]** Dropping `getDisplayText`/`isStreaming` changes the render path of a live, virtualized component. → Mitigation: `getDisplayText(segment)` → `segment.text` is a 1:1 swap when streaming is inactive (always, post-parent); `summary-render.spec.ts` covers multi-block rendering; the new `computeDisplayText` pass-through test covers the text-processing layer `TranscriptSegment` uses (see D4 amendment for why this is a pure-function test, not a render test).

- **[Transcript field removal breaks a reader]** Removing `sequence_id?`/`chunk_start_time?`/`is_partial?` could break a reader that destructures them. → Mitigation: D9 — every reader is inside the dead buffer/sort code removed in §3; `tsc --noEmit` catches any miss.

- **[Fixture migration breaks `loader.test.ts`]** → Mitigation: D3 — local `FixtureSegment` matches the existing 8 fields exactly; tests assert structure, not the type name.

- **[Hidden caller emerges since the panel]** → Mitigation: D6 apply-time re-verification; the falsification lens already CONVERGED on the current tree.

## Migration Plan

Single-PR. Ordered tasks (each is "write the failing/verifying test, then make the change"):

1. Apply-time re-verification (D6).
2. Strip dead listeners + cascading prop/state (`onTranscriptionError` chain, `transcriptionErrors`).
3. TranscriptContext surgery + stale imports + recording-started listener Vitest test.
4. Delete `transcriptService.ts`.
5. Delete dead types + stale imports + `Transcript` dead fields.
6. Delete `TranscriptView.tsx` entirely.
7. Streaming machinery removal (`useTranscriptStreaming.ts` + `VirtualizedTranscriptView` + both panels).
8. Fixture migration + prompt-injection split (structural test targets `TranscriptSegment`; repo-wide grep).
9. Rust corpses + `post_processor.rs` + IPC-absence unit test.
10. Stale copy fix at `VirtualizedTranscriptView.tsx:323`.
11. Cheap ride-alongs (event-bus example, README prose).
12. Spec sync (2 specs) + full §7 merge gate + concrete manual check.

**Rollback:** `git revert` the merge commit. No data migrations, no storage changes.

## Open Questions

None. All forks resolved: streaming machinery IN, Transcript dead fields IN, audio-recording-quality spec delta IN (all user decisions 2026-07-18).
