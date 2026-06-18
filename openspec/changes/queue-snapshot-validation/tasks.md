## 1. Boundary normalizer (TDD)

- [x] 1.1 Write a failing Vitest test: `normalizeQueueSnapshot({ manual_pause_all: true })` (missing `jobs`) returns `{ jobs: [], manual_pause_all: true }` and does not throw (adversarial: missing required field).
- [x] 1.2 Write a failing Vitest test: `normalizeQueueSnapshot(undefined)` and `normalizeQueueSnapshot(null)` both return `{ jobs: [], manual_pause_all: false }` (adversarial: absent payload).
- [x] 1.3 Write a failing Vitest test: `normalizeQueueSnapshot({ jobs: "nope", manual_pause_all: "yes" })` returns `{ jobs: [], manual_pause_all: false }` (adversarial: wrong types / schema mismatch).
- [x] 1.4 Write a failing Vitest test: a well-formed payload `{ jobs: [<valid QueueJob>], manual_pause_all: false }` is returned structurally unchanged (no field dropped, no value coerced).
- [x] 1.5 Implement and export `normalizeQueueSnapshot(payload: unknown): QueueSnapshot` in `frontend/src/services/queueService.ts`. Coerce a non-array `jobs` to `[]` and a non-boolean `manual_pause_all` to `false`; pass valid payloads through.
- [x] 1.6 Route `getQueueState()` (the `invoke` result) and `onQueueChanged()` (the event payload) through `normalizeQueueSnapshot` before they reach consumers. Also added a wiring test (`getQueueState applies the normalizer at the boundary`) so removing the normalizer call regresses a test, not just the isolated-function tests.

## 2. Smoke spec (UI-affecting change deliverable per CLAUDE.md §3)

- [x] 2.1 Write `frontend/e2e/smoke/queue-snapshot-validation.spec.ts`: with the queue mock returning a malformed payload (missing `jobs`), load the app, render the Meeting Notes sidebar items, and assert they render with no uncaught `pageerror`. Uses the existing mock-seam dispatcher to make `get_queue_state` return `{ manual_pause_all: true }` (no `jobs`).
- [x] 2.2 Run `pnpm exec playwright test e2e/smoke/queue-snapshot-validation.spec.ts` locally (chromium on Windows) and confirm green. NOTE: the home page is gated behind a no-microphone permissions alert in the smoke env, so meeting items don't render on `/` and a DOM-text assertion is unreliable. The spec instead emits a malformed `transcription-queue-changed` event through the mock bus (the `useRetranscriptionProgress` listener's `snapshot.jobs.filter(...)` is the reliable crash trigger) and asserts the emit does not throw + no `pageerror`. Verified RED→GREEN: reverting the `onQueueChanged` normalization makes the spec fail with `"Cannot read properties of undefined (reading 'filter')"`; restoring it passes (3.7s).

## 3. OpenSpec workflow

- [x] 3.1 `openspec validate queue-snapshot-validation` passes.
- [ ] 3.2 Archive the change once tasks are green; the delta spec syncs into `openspec/specs/post-meeting-pipeline/spec.md`.
