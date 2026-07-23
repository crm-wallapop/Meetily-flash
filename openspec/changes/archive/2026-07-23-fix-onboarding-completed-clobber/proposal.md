## Why

After completing first-run setup, the onboarding flow reappears on roughly every other launch. Root cause: `OnboardingContext.tsx`'s auto-save effect fires 1 second after mount with `completed: false` (the `useState` default), racing ahead of the async `loadOnboardingStatus`. Because `loadOnboardingStatus` awaits the slow `verifyModelStatus` probes (`parakeet_init`, `builtin_ai_get_available_summary_model`) BEFORE calling `setCompleted`, the 1-second auto-save wins the race on any cold start, overwriting the store's `completed: true` with `completed: false`. The guard at line 188 then blocks the re-save of `completed: true`, so the clobber sticks — the next launch reads `completed: false` and shows setup again.

This is pre-existing (not introduced by any recent change) but surfaces reliably on cold Vulkan dev starts where model verification is slow. There is currently no spec codifying the onboarding-persistence requirement, which is why the race went uncaught.

## What Changes

- In `loadOnboardingStatus`, set `completed` and `currentStep` from the fast `get_onboarding_status` invoke response BEFORE awaiting the slow `verifyModelStatus` probes. `completed` has zero dependency on model verification (`verifyModelStatus` line 353 passes it through unchanged), and `currentStep`'s only post-invoke logic is a clamp (lines 356-358) that can be applied inline. Moving these two `setState` calls earlier is behavior-preserving for the post-load state — but it means the existing guard at line 188 (`if (completed ...) return`) blocks the auto-save's clobber attempt naturally, with no gate, ref, or helper required.
- Add `frontend/e2e/smoke/fix-onboarding-completed-clobber.spec.ts` — mandatory per CLAUDE.md §3 (user-visible frontend change). Mocks `get_onboarding_status` returning `{completed: true}`, asserts `<OnboardingFlow>` stays hidden across a reload.
- Introduce a new `onboarding` capability spec with the requirement that completed-status persists across launches.

Non-goals: redesigning the onboarding flow; changing what `complete_onboarding` persists; touching the Rust `onboarding.rs` store layer (correct as-is); gating `model_status.parakeet` / `.summary` against the race — these stay default-false until verification resolves, but `verifyModelStatus` re-checks from disk on every launch (line 306 comment: "Don't trust saved status") and `complete_onboarding` always writes both to `"downloaded"`, so they self-heal next launch; adding on-disk migration for users already affected (the store self-heals: once the early-set lands, the next completion sticks); adding a `hasLoadedRef` gate + `shouldAutoSave` pure helper — considered and rejected (see design.md D2).

## Capabilities

### New Capabilities

- `onboarding`: Governs first-run setup state — specifically that the "completed" flag, once written by `complete_onboarding`, survives subsequent launches. Scoped tightly to status persistence in this change; future changes may add requirements for the step flow, model-download gating, and permission checks.

### Modified Capabilities

_(none — no existing spec covers onboarding)_

## Impact

- **Code**: `frontend/src/contexts/OnboardingContext.tsx` — in `loadOnboardingStatus` (lines 300-322), move the `setCompleted` + `setCurrentStep` calls to immediately after the fast invoke success branch (line 303), before `await verifyModelStatus(status)` at line 307. Apply the same `> 4 ? 3 : ...` clamp inline. Leave `setParakeetDownloaded` / `setSummaryModelDownloaded` after verification (they depend on it). ~5-line diff.
- **Tests**: `frontend/e2e/smoke/fix-onboarding-completed-clobber.spec.ts` — new Playwright smoke spec mocking the store read. This is the only automated test that exercises the actual race wiring end-to-end.
- **Spec**: `openspec/specs/onboarding/spec.md` — new capability with the persistence requirement.
- **User-visible behavior**: after completing setup once, the setup screen no longer reappears on subsequent launches. Users currently seeing the repeat-prompt will see it one final time; after re-completing, it sticks.
- **layout.tsx**: independently reads the same store at line 84 via a single fast invoke (no model verification) — not modified. Its read is correct; the bug is the write side in OnboardingContext.
- **No breaking changes** to IPC contracts, storage schema, or the Rust store layer. The Tauri `onboarding-status.json` format is unchanged.
