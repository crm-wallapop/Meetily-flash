# Tasks

## 1. Cancel-on-blur + chip focus guard (the fix)

- [x] 1.1 **(red)** Smoke: open the speaker-name input, click elsewhere in the
  document → assert the input is gone AND no `label_speaker` call was
  dispatched. Currently fails: input stays open.
- [x] 1.2 **(red → adversarial: no accidental commit)** open the input, type
  "Alice", then click outside → assert `label_speaker` was **not** dispatched
  (the typed name is discarded, not committed). Protects against the
  submit-on-blur regression.
- [x] 1.3 **(red → adversarial: suggestion chip still submits)** open the
  input, click a suggestion chip → assert `label_speaker` IS dispatched with
  the chip's name. **Setup required:** populate `window.__smokeSpeakers` via
  `page.addInitScript` with at least one named speaker (e.g.,
  `[{ id: 's1', name: 'Alice', color: 'hsl(137, 65%, 55%)' }]`) before
  navigation, so `useSpeakerRename` populates `knownSpeakers` and chips render.
  Without this fixture (the existing `speaker-diarization.spec.ts` 15.2 test
  omits it), `knownSpeakers` is empty and no chips appear — the test cannot
  be written. The fixture names must NOT start with `"Speaker "` (filtered out
  by `useSpeakerRename`).
- [x] 1.4 **(green)** In `SpeakerBadge.tsx`:
  - add `onBlur={onCancel}` to the `<input>` (line ~113)
  - add `onMouseDown={(e) => e.preventDefault()}` to each suggestion-chip
    `<button>` (line ~134)
  - tests 1.1–1.3 pass.
- [x] 1.5 **(green, regression)** Escape still cancels; Enter with text still
  submits (`label_speaker` dispatched). Assert both in the same smoke file so
  the blur change is proven non-regressive on the existing keyboard paths.

## 2. Smoke spec deliverable + spec update + archive gate

- [x] 2.1 **Create `frontend/e2e/smoke/speaker-rename-cancel.spec.ts`** as the
  explicit smoke deliverable for this change (CLAUDE.md §3 requires
  `e2e/smoke/<change-name>.spec.ts`). The `.githooks/pre-push` hook derives
  `SPEC="e2e/smoke/${CHANGE}.spec.ts"` from the branch name
  (`fix/speaker-rename-cancel` → `e2e/smoke/speaker-rename-cancel.spec.ts`);
  putting these tests in the existing `speaker-diarization.spec.ts` instead
  would cause the hook to silently skip Playwright. To avoid duplicating the
  ~67-line `bootstrap()` + `speakerCalls()` setup, extract them into a shared
  `e2e/smoke/_speaker-helpers.ts` imported by both specs (DRY).
- [x] 2.2 Update `openspec/specs/speaker-diarization/spec.md` — add the
  cancel-on-blur requirement per this change's delta spec (it amends the
  existing "Retroactive speaker labeling via inline badges" requirement's
  inline-input behavior, which is silent on dismiss mechanics). Applied by
  `/opsx:archive` (the delta spec is the canonical change; archive folds it in).
- [x] 2.3 **Before `/opsx:archive`:** re-read
  `specs/speaker-diarization/spec.md` and `design.md`; amend if the
  implementation evolved during apply. Re-read 2026-06-28: no behavioral
  evolution — the delta's cancel-on-blur scenarios still hold (a later change,
  per-turn-speaker-override, moved `onBlur` from the input to the container, but
  the cancel-when-focus-leaves behavior is preserved; smoke 1.1–1.5 green).
- [x] 2.4 Run the merge gate: `cargo test && pytest && pnpm test && pnpm lint`.
  Smoke IS required for this change (user-visible frontend behavior) — the
  `speaker-rename-cancel.spec.ts` from 2.1 is the deliverable. Status (re-verified
  at archive): vitest 237/237 ✓, eslint clean ✓, smoke 8/8 (both speaker specs) ✓,
  cargo speaker 11/11 ✓. Frontend-only change (no Rust/Python touched), so full
  cargo/pytest are no-op regressions for this delta; the speaker cargo suite is
  green.

## 3. Test-level choice (documented, not a code task)

This change is pure DOM-event wiring (`onBlur` + `onMouseDown preventDefault`).
There is no pure decision function to extract — the logic IS the event
ordering. Per the project's hook-testing convention (extract pure helpers, no
`renderHook` — `@testing-library/react` is not a dependency), forcing a
Vitest unit test of DOM-event ordering would require either an artificial
pure-function extraction (KISS violation) or `renderHook` (forbidden). The
faithful test of event ordering is the Playwright smoke spec (real browser
DOM), which is therefore the appropriate and sole test level for this change.
The `pnpm test` (Vitest) gate does not cover this change by design; the
`pnpm test:smoke` gate and the pre-push scoped smoke do.
