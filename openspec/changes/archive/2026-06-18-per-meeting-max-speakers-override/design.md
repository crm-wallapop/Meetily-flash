## Context

The `max_speakers` cap is currently **global**: a single value in the `settings` table (default 10, range [2, 20]), read once by `run_diarization_for_meeting` in `frontend/src-tauri/src/audio/speaker/commands.rs` and enforced by merging the most-isolated cluster until the cluster count is at or below the cap.

The motivating problem is that diarization structurally over-counts speakers. Seven independent counting methods (AHC, spectral eigengap, silhouette, BIC, ECAPA-TDNN, Sortformer embeddings, nemo_titanet-on-Sortformer-slots) all converge on 4 clusters for a known 3-speaker meeting — the 4th cluster is a real acoustic splinter, not a model artifact. Both the production AHC pipeline and the Sortformer alternative over-count; the wall is in the audio, not the algorithm. A user-facing cap is therefore the durable fix regardless of pipeline — but a single global cap cannot correct one meeting without distorting every other meeting.

Constraints: hexagonal architecture (CLAUDE.md §2), adversarial TDD (§4), local-first / zero new cloud deps.

## Goals / Non-Goals

**Goals:**
- A per-meeting `max_speakers` override that takes precedence over the global default.
- `NULL` override = global default, so every existing meeting behaves exactly as before (zero behavior change on upgrade).
- The override applies to both initial diarization and re-diarization, via the single chokepoint (`run_diarization_for_meeting`).
- The cap remains an upper bound — diarization never splits clusters to reach the number.

**Non-Goals:**
- Solving the speaker-counting problem. The over-count is structural; this change gives the user an override, not a better counter. See [[diarization-single-model]] D13.
- Changing the most-isolated-cluster merging algorithm.
- Auto-rediarizing when the override changes (destructive — clears manual labels).
- Removing the global `settings.max_speakers` setting; it remains the default.

## Decisions

**D1 — Storage: nullable `meetings.max_speakers INTEGER`.**
The override is a single nullable column on the `meetings` table. SQL `NULL` naturally means "inherit the global default," and the fallback is a single `COALESCE(m.max_speakers, s.max_speakers)`. Alternatives considered: a separate `meeting_diarization_settings` table (over-normalized for one nullable int), a JSON settings blob on `meetings` (untyped, harder to validate), or override rows in a key-value table (more complex for no gain). The nullable column is the simplest thing that satisfies the spec.

**D2 — Resolution: per-meeting override if NOT NULL, else global, resolved at diarization time.**
`run_diarization_for_meeting` reads the effective cap with `COALESCE(meetings.max_speakers, settings.max_speakers)`, replacing the current `SELECT max_speakers FROM settings`. The resolved value is NOT stored denormalized — a stored copy would go stale if the global default changed. Resolving at run time means changing the global default immediately affects every non-overridden meeting.

**D3 — Cap is an upper bound, not a target.**
The merge loop runs `while centroids.len() > effective_cap.max(2)`. A meeting whose natural cluster count is at or below the cap is untouched; clusters are never split to reach the configured number. (This documents existing behavior explicitly so it is testable.)

**D4 — Apply via the existing Re-diarize action; do NOT auto-rediarize on override change.**
Re-diarization is destructive: per the "Re-diarization cleans up stale state" requirement it clears ALL speaker labels, including manual corrections, and costs ~1 min. Auto-triggering it from a settings control would silently destroy the user's manual corrections. The override control lives in the meeting's speaker panel next to the existing Re-diarize button: the user sets the cap, then explicitly re-diarizes. Alternative (auto-rediarize on change) rejected as surprising and destructive.

**D5 — Tauri command surface.**
- `set_meeting_max_speakers(meeting_id: String, cap: Option<i32>)` — validates the meeting exists and, when `Some`, that `cap` is in [2, 20]; `None` clears the override (sets the column to NULL).
- `get_meeting_max_speakers(meeting_id: String) -> MeetingMaxSpeakers { override: Option<i32>, effective: i64, global_default: i64 }` — returns all three so the UI renders "Auto (default: N)" vs an explicit number in a single round-trip.

**D6 — Hexagonal boundaries.**
No new port is introduced. Cap resolution is a settings read inside the use case (`run_diarization_for_meeting`), identical in layer to the current global read it replaces. The two new commands are thin Tauri wrappers (parse request → repository → return), matching the existing `set_max_speakers` / `get_max_speakers` pair. On the frontend, adapter functions live in `speakerService.ts` and the control is a React component that calls the adapter — never `invoke()` directly (CLAUDE.md §2c).

## Risks / Trade-offs

- **[Override silently ignored if the user forgets to re-diarize]** → The UI shows the effective value and the Re-diarize button is adjacent. The override is persisted regardless, so any later diarization/re-diarization picks it up.
- **[Two settings (global + per-meeting) confuse users]** → The per-meeting control is labelled "Max speakers (this meeting)" with an explicit "Auto (use default: N)" option; the global setting stays in settings as the default. `get_meeting_max_speakers` returns both so the UI never has to infer.
- **[Column added to a `meetings` table that may be large]** → A nullable `ALTER TABLE … ADD COLUMN` on SQLite is metadata-only (no rewrite, no backfill); negligible cost.

## Verification

Four layers, each covering what the layer below cannot:

1. **Pure unit (`cargo test`, no I/O).** `resolve_effective_cap` (override-wins, NULL-falls-back, global-flows-through); `enforce_max_speakers_cap` (merges most-isolated cluster, no-op above count) on hand-built centroids; command validation (range [2,20], unknown-meeting rejection, NULL-clear, `get` shape). These pin the cap-resolution and merge logic in isolation.
2. **Integration (`cargo test`, no GPU/DB/audio).** `cluster_then_cap_*` wires the real AHC `cluster_by_centroids` into the real `enforce_max_speakers_cap` on synthetic 4-cluster embeddings and asserts the cap reduces 4→3 (and is a no-op above the count). This closes the gap that layer 1 leaves: the two functions were only ever tested apart, never composed. Enabling it required widening `cluster_by_centroids` to `pub(crate)` (D6 still holds — no new port, just visibility so the clustering use case is reachable from the commands module's tests).
3. **Full pipeline (`#[ignore]`, `--features vulkan`, real prod DB).** `test_per_meeting_override_caps_speakers` runs the real 95db audio through nemo_titanet → AHC → cap with override=3 and asserts ≤3, restoring the DB afterwards. This is the only layer that exercises the real model's embedding geometry; layers 1–2 use synthetic data and cannot detect a model regression. Kept `#[ignore]` because the vulkan build + DB write make it unsuitable for the default gate.
4. **UI smoke (`pnpm test:smoke`).** `e2e/smoke/per-meeting-max-speakers-override.spec.ts` proves the control renders in Auto mode, that setting a number dispatches `set_meeting_max_speakers {cap:number}`, and that Auto dispatches `{cap:null}`. Stateful handlers live in the shared `e2e/smoke/_meeting-details.ts` fixture because every meeting-details spec mounts this control.

## Migration Plan

1. Migration `2026XXXX_per_meeting_max_speakers.sql`: `ALTER TABLE meetings ADD COLUMN max_speakers INTEGER;` (nullable, no default). All existing meetings get NULL → global behavior → identical results.
2. Rust: change the cap read in `run_diarization_for_meeting` to `COALESCE`; add `set_meeting_max_speakers` / `get_meeting_max_speakers`; register both in `lib.rs`.
3. TypeScript: adapter functions in `speakerService.ts`; the control in the meeting speaker panel.
4. Rollback: drop the column and revert the code. No data-loss risk because NULL == prior behavior.

## Open Questions

None blocking. The auto-rediarize-on-change question is settled as Non-Goal D4; revisit only if users report the two-step (set cap → click Re-diarize) as friction.

Note: this change modifies a requirement introduced by the in-flight `speaker-diarization` change (not yet in canonical `openspec/specs/`). Archive `speaker-diarization` before this change so the MODIFIED requirement resolves against the promoted spec.
