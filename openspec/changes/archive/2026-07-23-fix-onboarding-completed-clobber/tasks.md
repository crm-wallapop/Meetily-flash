## 1. Set fast-derived state early (design D1)

- [x] 1.1 In `frontend/src/contexts/OnboardingContext.tsx`, `loadOnboardingStatus` (lines 300-322): move `setCompleted` and `setCurrentStep` to immediately after the `if (status) {` branch (line 303), BEFORE `await verifyModelStatus(status)` (line 307). Apply the same `> 4 ? 3 : current_step` clamp inline (the clamp currently lives at `verifyModelStatus` lines 356-358 — replicate it on the fast path).
- [x] 1.2 Remove the now-duplicate `setCurrentStep(verifiedStatus.currentStep)` / `setCompleted(verifiedStatus.completed)` calls at lines 309-310. Leave `setParakeetDownloaded(verifiedStatus.parakeetDownloaded)` / `setSummaryModelDownloaded(verifiedStatus.summaryModelDownloaded)` where they are (they depend on verification). `verifyModelStatus` is cleaned up to its actual job: drop the now-unused `savedStatus` parameter and the dead `completed`/`currentStep`/clamp computation (made unused by this change, removed per §6), return only `{ parakeetDownloaded, summaryModelDownloaded }`. The `> 4 ? 3` clamp is now single-sourced on the fast path.

## 2. Mandatory smoke test (CLAUDE.md §3)

- [x] 2.1 Create `frontend/e2e/smoke/fix-onboarding-completed-clobber.spec.ts`:
  - **Case A (race fix):** mock `invoke('get_onboarding_status')` → `{completed: true, current_step: 4, model_status: {parakeet: "downloaded", summary: "downloaded"}, ...}`; load the app; assert `<OnboardingFlow>` is NOT in the DOM.
  - **Case B (first-launch):** mock `invoke('get_onboarding_status')` → `null`; load the app; assert `<OnboardingFlow>` IS shown.
  - Use the webpack mock-alias seam (gated on `PLAYWRIGHT_E2E=1`, injected by `playwright.config.ts`). Follow the established pattern in existing `e2e/smoke/*.spec.ts` files.

## 3. Merge gate (pre-archive)

- [x] 3.1 `cargo test` — PASS (after killing a stale `meetily-flash.exe` PID 74016 that was locking the debug binary; environmental, not from this TS-only change).
- [x] 3.2 `pytest backend/ -m "not slow"` — PASS (6 tests).
- [x] 3.3 `pnpm test` — PASS (262/262 existing Vitest tests; no regressions).
- [x] 3.4 `pnpm lint` — PASS (warnings only, all pre-existing in other files; none in `OnboardingContext.tsx`).
- [x] 3.5 `pnpm exec playwright test e2e/smoke/fix-onboarding-completed-clobber.spec.ts` — PASS (2/2 on chromium: the race-fix call-log assertion + the first-launch DOM assertion). Killed stale dev server PID 115740 on :3118 first per the task note.
