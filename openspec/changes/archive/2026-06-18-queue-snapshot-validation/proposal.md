## Why

Expanding the Meeting Notes sidebar crashes the app with
`Cannot read properties of undefined (reading 'find')` whenever the
queue-state IPC payload arrives without a `jobs` array. The frontend adapter
(`frontend/src/services/queueService.ts`) trusts the Tauri payload via a bare
TypeScript cast and stores it straight into React state; the first
`snapshot.jobs.find(...)` then throws. Three consumers crash unguarded: the
Sidebar item renderer (`components/Sidebar/index.tsx:653`), `useQueueJob`
(`hooks/useQueueJobStatus.ts:97`), and `useRetranscriptionProgress`
(`hooks/useQueueJobStatus.ts:59`).

The Rust `QueueSnapshot` is always constructed with both fields
(`use_cases/transcription_queue.rs`), so steady-state production payloads are
well-formed. The crash is still real: it reproduces whenever the payload shape
diverges from the cast — version skew between a hot-reloaded frontend and a
stale Rust binary, a partial payload, or a mock (the smoke harness hit it
during `cross-platform-automated-smoke-tests`). CLAUDE.md §9 treats all IPC
output as untrusted input validated at the boundary; the bare cast violates
that.

## What Changes

- Add a pure `normalizeQueueSnapshot(payload)` normalizer in the queue adapter
  that coerces a missing/non-array `jobs` to `[]` and a missing/non-boolean
  `manual_pause_all` to `false`, passing valid payloads through unchanged.
- Route both boundary entry points — `getQueueState()` (invoke) and
  `onQueueChanged()` (event listener) — through the normalizer, so React
  state always holds a well-formed `QueueSnapshot` regardless of payload.
- No Rust changes: the Rust `QueueSnapshot` is already correct; this is a
  frontend boundary-resilience fix.

## Impact

- Affected files: `frontend/src/services/queueService.ts` (normalizer + two
  call sites), a new Vitest unit-test file, a new smoke spec.
- No data migration, no schema change, no backend change.
- User-visible: the sidebar no longer crashes on a malformed queue payload;
  behavior is otherwise identical (valid payloads render exactly as before).
