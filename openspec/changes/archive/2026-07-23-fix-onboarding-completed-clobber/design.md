## Context

`OnboardingContext.tsx` mounts with `completed = false` (the `useState` default). Two effects race on mount:

1. **Auto-save effect (lines 183-197)** — schedules a 1-second debounced `saveOnboardingStatus()` whenever `completed` is falsy. On mount, `completed` is `false`, so the 1-second timer starts immediately.
2. **Load effect (lines 101-118)** — calls `loadOnboardingStatus()`, which invokes `get_onboarding_status` (fast, ~5-50ms) THEN awaits `verifyModelStatus()` — the slow path that runs `parakeet_init` + `parakeet_has_available_models` + `builtin_ai_get_available_summary_model` (many seconds on a cold Vulkan start). `setCompleted(verifiedStatus.completed)` runs only AFTER those probes resolve (currently line 310).

On any launch where the load takes >1 second, the auto-save fires first and writes `{ completed: false, currentStep: 1, ... }` to the Tauri store, clobbering the persisted `completed: true`. When the load eventually resolves and sets `completed = true` in React state, the auto-save effect re-runs but the guard at line 188 (`if (completed ...) return`) blocks any re-save — so the clobber sticks.

The key insight: `completed` has zero dependency on the slow model probes. `verifyModelStatus` line 353 passes `completed` through unchanged (`const completed = savedStatus.completed;`); it only mutates `parakeetDownloaded` / `summaryModelDownloaded`. So `setCompleted` can run immediately after the fast invoke, long before the 1-second debounce fires. The same is true of `currentStep` — its only post-invoke logic is a clamp (`> 4 ? 3 : x`) at lines 356-358, trivially applied inline.

The Rust store layer (`onboarding.rs`) is correct. The bug is purely client-side: `setCompleted` runs too late.

## Goals / Non-Goals

**Goals:**
- Make `completed` reflect the persisted store value before the auto-save debounce can fire, so the existing line-188 guard blocks the clobber naturally.
- Pin the persistence requirement in a new `onboarding` spec + a Playwright smoke test so the race can't silently regress.

**Non-Goals:**
- Redesigning the onboarding step flow, model-download gating, or permission checks.
- Touching the Rust `onboarding.rs` store layer.
- Gating `model_status.parakeet` / `.summary` against the race — self-heals next launch.
- Retroactively repairing already-clobbered stores (see D3).
- Adding a gate ref / pure-helper extraction (see rejected D2).

## Decisions

### D1: Set fast-derived state early (the chosen fix)

In `loadOnboardingStatus` (lines 300-322), move the `setCompleted` and `setCurrentStep` calls to BEFORE the `await verifyModelStatus(...)` line:

```ts
const status = await invoke<OnboardingStatus | null>('get_onboarding_status');
if (status) {
  setCompleted(status.completed);
  setCurrentStep(status.current_step > 4 ? 3 : status.current_step);

  const verifiedStatus = await verifyModelStatus();
  setParakeetDownloaded(verifiedStatus.parakeetDownloaded);
  setSummaryModelDownloaded(verifiedStatus.summaryModelDownloaded);

  await checkActiveDownloads();
}
```

When the 1-second auto-save timer fires, `completed` is already correct, so the existing guard at line 188 blocks the write. `verifyModelStatus` is reduced to its actual job — probing disk for the two model flags — and now returns only `{ parakeetDownloaded, summaryModelDownloaded }`. Its `savedStatus` parameter and the dead `completed`/`currentStep`/clamp computation (made unused by moving `setCompleted`/`setCurrentStep` to the caller) are removed per CLAUDE.md §6 (delete unused code when certain), leaving the `> 4 ? 3` clamp single-sourced on the fast path.

**Why this beats a gate (KISS):** the gate approach solves the symptom (auto-save fires too early) rather than the cause (`completed` is set too late). Eliminating the race at the source means the existing guard does all the work — no new state, no new tests for synthetic decision functions.

### D2 (REJECTED): hasLoadedRef gate + shouldAutoSave pure helper

Considered first; rejected after shark-tank review. Would have added `hasLoadedRef = useRef(false)` flipped in a `finally` block, an exported `shouldAutoSave({ hasLoaded, completed, isCompleting })` pure helper, and a 5-case Vitest suite.

Rejected because:

- **KISS / YAGNI violation:** ~40 lines (ref + finally + helper + 5 unit tests) vs D1's ~5-line diff. Same observable guarantee.
- **`finally` is redundant:** `loadOnboardingStatus`'s catch at lines 319-321 logs and does NOT re-throw, so `finally` is equivalent to "the line after the catch." Defending against a hypothetical future re-throw is YAGNI.
- **The pure-helper test is load-bearing on the wrong thing:** it tests a synthetic decision function with synthetic inputs and does NOT exercise the actual race wiring. The only automated check that pins the end-to-end "completed stays true across reload" path is the smoke test — which is mandatory under CLAUDE.md §3 regardless. So the helper adds test surface without adding protection.
- **No extra protection in practice:** both D1 and D2 protect `completed` + `currentStep`. D2 additionally protects `model_status.parakeet` / `.summary`, but those self-heal next launch (re-verified from disk), so the extra protection has no user-visible value.

### D3: No retroactive store repair

Don't add migration logic to heal already-clobbered stores. Once D1 lands, the next `complete_onboarding` call sticks, so affected users simply complete setup one final time.

**Why:** the store is self-healing after the fix. A migration adds version detection + one-shot logic for a one-time inconvenience — complexity not justified by the payoff.

## Risks / Trade-offs

- **[Risk] `verifyModelStatus` throws after `setCompleted` ran.** → `completed` is already correct in state; the catch at 319-321 swallows; no clobber. No special handling needed.
- **[Risk] The fast `get_onboarding_status` invoke itself throws (transient store failure).** → `setCompleted` never runs; `completed` stays at the `useState` default `false`. The 1-second auto-save fires and writes `completed: false`, which clobbers a real `completed: true` if one existed on disk. **Accepted trade-off:** if the store read itself fails, no client-side mechanism can know what was on disk; the user re-completes once and D3 self-heal applies. Note: D2 has the same failure mode. Spec scenario 4 amended to reflect this honestly (it previously asserted "SHALL NOT silently clobber," which is unsatisfiable by any mechanism without a successful read).
- **[Risk] `parakeet_init` hangs indefinitely (neither resolves nor rejects).** → `setCompleted` already ran (before the hang). The 1-second auto-save fires with correct `completed`; guard blocks. The hang only blocks model-verification flags, which self-heal next launch. **No special handling needed** — D1 is hang-safe by construction because the critical state is set before the slow await. (This dissolves a concern that D2 could only address with a `Promise.race` timeout.)
- **[Risk, out of scope] User completes onboarding during the slow load window.** → `loadOnboardingStatus`'s stale `status.completed` (false) could revert the just-set true when its `setCompleted` runs. Under D1, the vulnerable window is ~5-50ms (the fast invoke round-trip) vs D2's multi-second window, so D1 strictly reduces the probability. Fully fixing requires cancelling the in-flight load on completion, which is a different bug (stale-load-during-completion). **Filed separately** per scoped-change discipline; not blocked by this change.
- **[Risk, pre-existing] React 18 StrictMode dev double-mount.** → Mount effect runs twice; the first invocation's pending `loadOnboardingStatus` resolves into a discarded fiber. Pre-existing wrinkle, not introduced by D1. Not addressed here.
- **[Trade-off] One-time UX hiccup for currently-affected users** (they see the prompt once more, re-complete, then it sticks). Acceptable vs migration complexity (D3).

## Adversarial test plan (§4 categories applicable)

- **Race / timing (the core bug):** pinned by the smoke test — mock `get_onboarding_status → {completed: true}`, reload, assert `<OnboardingFlow>` stays hidden. This is the only test that exercises the actual race wiring end-to-end.
- **Genuine first launch:** smoke test variant — mock `get_onboarding_status → null`, assert `<OnboardingFlow>` shown.
- **Error path (store-read throw):** accepted trade-off (risk R2), not asserted as a no-clobber guarantee.
