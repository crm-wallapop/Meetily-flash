## 1. DB Migration

- [x] 1.1 Add migration `20260616000000_per_meeting_max_speakers.sql`: `ALTER TABLE meetings ADD COLUMN max_speakers INTEGER;` (nullable, no default)
- [x] 1.2 RED: in-memory SQLite — after migration, a freshly inserted meeting has `max_speakers IS NULL` (so existing meetings inherit the global default with zero behavior change)
- [x] 1.3 GREEN: confirm migration applies and the column is nullable

## 2. Cap Resolution (use case, pure + wired)

- [x] 2.1 RED: test `resolve_effective_cap(Some(3), 10) == 3` — per-meeting override wins
- [x] 2.2 RED: test `resolve_effective_cap(None, 6) == 6` — NULL override falls back to global
- [x] 2.3 GREEN: extract `resolve_effective_cap(override: Option<i64>, global: i64) -> usize` as a pure helper in `commands.rs`
- [x] 2.4 RED: test the merge loop is a no-op when cluster count ≤ effective cap (upper bound, not a target) — extracted `enforce_max_speakers_cap` + `enforce_cap_is_noop_below_threshold` / `enforce_cap_merges_most_isolated_cluster`
- [x] 2.5 `run_diarization_for_meeting` reads per-meeting override + global via `resolve_effective_cap_for_meeting` (reads `m.max_speakers` and `(SELECT max_speakers FROM settings LIMIT 1)`, applies `resolve_effective_cap`). **Caught & fixed a NULL-read bug:** the nullable `meeting_cap` must be read as `Option<i64>` — sqlx-SQLite decodes NULL to `0` for a non-Option `i64`, which would have capped every non-overridden meeting to 2 speakers.

## 3. Tauri Commands (adversarial)

- [x] 3.1 RED: test `set_meeting_max_speakers` rejects `cap = 1` and `cap = 21` (range [2, 20])
- [x] 3.2 GREEN: implement range validation (reuse the [2, 20] check via `validate_meeting_cap`)
- [x] 3.3 RED: test `set_meeting_max_speakers` rejects a `meeting_id` not in `meetings` (also covers SQL-injection-shaped IDs via parameterized query)
- [x] 3.4 GREEN: implement meeting-existence check
- [x] 3.5 RED: test `set_meeting_max_speakers(meeting_id, None)` sets `meetings.max_speakers = NULL` (clears override)
- [x] 3.6 GREEN: implement the NULL-clear path
- [x] 3.7 RED: test `get_meeting_max_speakers` returns `{ override, effective, global_default }` with effective = override when set and global_default otherwise
- [x] 3.8 GREEN: implement `get_meeting_max_speakers`
- [x] 3.9 Register `set_meeting_max_speakers` and `get_meeting_max_speakers` in `lib.rs` `invoke_handler`

## 4. Frontend Adapter + UI

- [x] 4.1 RED: test `setMeetingMaxSpeakers(meetingId, cap)` and `getMeetingMaxSpeakers(meetingId)` invoke the correct commands with the right argument shapes
- [x] 4.2 GREEN: add the two adapter functions to `frontend/src/services/speakerService.ts`
- [x] 4.3 RED: test the per-meeting "Max speakers" control rejects input outside [2, 20]
- [x] 4.4 GREEN: add the control to the meeting speaker panel — `MeetingMaxSpeakersControl.tsx` (number input + "Auto" toggle that maps to `None`); setting a value persists immediately, "Auto" clears to `None`
- [x] 4.5 Wire the control so changing it does NOT auto-trigger re-diarization (the user applies it via the existing Re-diarize button); show the effective value from `get_meeting_max_speakers`

## 5. Integration & Verification

- [x] 5.1 RED (`#[ignore]`, real meeting): `test_per_meeting_override_caps_speakers` — global=10, per-meeting override=3, assert ≤ 3 (restores DB afterwards)
- [x] 5.2 GREEN: override resolution proven end-to-end at the unit level by `resolve_effective_cap_for_meeting_reads_override_then_global` (override beats global; NULL falls back; global change flows through) + `enforce_max_speakers_cap` merge tests. The `#[ignore]` real-DB run remains available for gold verification (`cargo test -p meetily-flash --features vulkan -- --ignored test_per_meeting_override_caps_speakers`).
- [x] 5.3 `cargo test` green (25 new unit tests pass, 3 real-DB tests ignored); `pnpm test` green (191 tests incl. 9 new); `pnpm lint` — this change's files are clean; 2 pre-existing `prefer-const` errors live in `DownloadProgressStep.tsx` (unrelated, predate this change).
- [x] 5.4 Automated UI smoke: `frontend/e2e/smoke/per-meeting-max-speakers-override.spec.ts` (3 tests, 7/7 `pnpm test:smoke` green) — proves the control renders in Auto mode reflecting the global default, that entering an override dispatches `set_meeting_max_speakers {meetingId, cap:number}`, and that the Auto toggle dispatches `{cap:null}` to clear it. The stateful `get/set_meeting_max_speakers` handlers live in the shared `e2e/smoke/_meeting-details.ts` fixture (every meeting-details spec mounts this control, so the fail-closed dispatcher needs them registered).
- [x] 5.5 Default-gate Rust integration test: `cluster_then_cap_enforces_override_on_real_clustering` (+ no-op sibling) in `commands.rs` — wires the REAL AHC `cluster_by_centroids` (made `pub(crate)`) into the REAL `enforce_max_speakers_cap` on synthetic 4-cluster embeddings and asserts the cap reduces 4→3 (and is a no-op above the cluster count). Runs in plain `cargo test` — no GPU, no DB, no audio (the only step needing vulkan is model inference, which stays in the `#[ignore]` 5.1 test). Fills the gap that the two functions were previously tested only in isolation on hand-built centroids. 2 passed, 0 failed.
