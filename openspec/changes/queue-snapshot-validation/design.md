## Context

The transcription queue exposes a `get_queue_state` command and emits
`transcription-queue-changed` Tauri events, both delivering a `QueueSnapshot`
(`{ jobs: QueueJob[], manual_pause_all: boolean }`). The frontend adapter
`frontend/src/services/queueService.ts` casts both payloads to `QueueSnapshot`
with no runtime validation and hands them to React state via
`useQueueSnapshot`. Three consumers then call `snapshot.jobs.find(...)`
unguarded: the Sidebar item renderer, `useQueueJob`, and
`useRetranscriptionProgress`. When `jobs` is absent, all three throw
`Cannot read properties of undefined (reading 'find')`.

The Rust `QueueSnapshot` is always constructed with both fields
(`use_cases/transcription_queue.rs` — `get_state` and every notifier emit
site at lines 553, 582, 683). Steady-state production payloads are therefore
well-formed. The crash is still real: it reproduces whenever the payload shape
diverges from the cast — version skew between a hot-reloaded frontend and a
stale Rust binary, a partial payload, or a mock (the smoke harness hit it
during `cross-platform-automated-smoke-tests`). CLAUDE.md §9 treats all IPC
output as untrusted input that must be validated at the boundary; the bare
cast violates that.

## Goals / Non-Goals

**Goals**
- Guarantee `snapshot.jobs` is always an array inside React state, regardless
  of payload shape, so no consumer can crash on `.find`.
- Fix the crash at a single boundary chokepoint (DRY), not at each call site.
- Preserve exact behavior for well-formed payloads (no semantic change to the
  happy path).

**Non-Goals**
- Deep-validating each `QueueJob`'s fields (`status`/`phase`/etc.). A
  malformed job entry does not crash consumers — it only produces a wrong
  label — so guarding it is scope creep beyond the observed crash. Deferred.
- Validating payloads with Zod. Blocked on the `frontend-zod-schemas`
  follow-up (CLAUDE.md §8); until then a hand-written normalizer is used, to
  be replaced one-for-one with `Schema.parse` when Zod lands.
- Changing the Rust `QueueSnapshot` shape or any emission logic.

## Decisions

### D1: Normalize at the adapter boundary, not at call sites

A single `normalizeQueueSnapshot(payload): QueueSnapshot` in `queueService.ts`
is applied in both `getQueueState()` and `onQueueChanged()`. This is the §9
boundary — the only place untrusted IPC data enters the frontend. Rejected
alternative: optional-chaining / `?.find()` guards at each of the three
consumer call sites. That scatters the defense across three places to keep in
sync, re-violates DRY, and leaves the malformed object sitting in React state
to crash the next unchecked consumer.

### D2: Coerce, don't reject

The normalizer coerces invalid input to safe defaults (`jobs: []`,
`manual_pause_all: false`) rather than throwing. Throwing would turn a
malformed payload into a different crash; coercing degrades gracefully (the
sidebar renders with no queue badges until a well-formed event arrives). This
matches the existing `EMPTY_SNAPSHOT` fallback the hook already seeds from.

### D3: Minimal field-validation scope

Validate only the two top-level fields (`jobs` must be an array;
`manual_pause_all` must be a boolean). Do not validate `QueueJob` entry shape
— that does not cause the observed crash and is left to the future Zod
follow-up. Guard the structurally-observed failure, not hypothetical ones.

## Adversarial test category applicability (CLAUDE.md §4)

| §4 Category | Applicable? | Where addressed |
|---|---|---|
| Malformed response / schema mismatch | Yes — IPC payload shape | Tasks 1.1–1.4 (unit) + 2.1 (smoke) |
| Missing required fields | Yes — missing `jobs` | Task 1.1 |
| Empty sections / SQL injection / path traversal / concurrent saves | No — different layer | — |
| Audio / transcription / LLM categories | No — different layer | — |

## Risks / Trade-offs

- **Silent swallowing of genuinely broken payloads.** Coercion hides a
  shape-mismatch that might indicate a real Rust bug. Mitigation: the
  normalizer is unit-tested to pass valid payloads through unchanged, so a
  Rust-shape regression still surfaces visibly ("queue badges never render")
  rather than as an invisible wrong value.
- **No deep `QueueJob` validation** means a payload with a structurally-wrong
  job entry renders a wrong badge label instead of failing. Accepted; covered
  when Zod lands.

## Migration Plan

Purely additive boundary normalization; no schema or behavior change for valid
payloads. Rollback: revert `queueService.ts`; the crash returns but no state
is corrupted.
