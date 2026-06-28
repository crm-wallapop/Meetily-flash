## 1. B3 Spike — GUI-subsystem log inheritance (gates §3)

- [ ] 1.1 On a `fix/stop-notification-ux` branch, temporarily drop the `not(debug_assertions)` conjunct in `frontend/src-tauri/src/main.rs:1-4` so `windows_subsystem = "windows"` applies in debug too.
- [ ] 1.2 Run `tauri dev` with `RUST_LOG=debug` and confirm (a) Rust logs still stream into the launching terminal via inherited stdio, and (b) no fresh console window is allocated for the primary dev process. Record the outcome in `design.md` Open Questions.
- [ ] 1.3 Trigger a `meetily://` activation while the dev app is running (via the `__dev_inject_deep_link` seam at `lib.rs:256`, or by clicking a notification action button) and confirm no console window flashes for the single-instance secondary.
- [ ] 1.4 If inherited stdio does NOT carry logs, spike the fallback (`AttachConsole(ATTACH_PARENT_PROCESS)` early in `main()`) and confirm it restores the dev terminal log experience without re-introducing the secondary console flash. Record the chosen path in `design.md` Decisions before §3.

## 2. B2 — Dev-build AUMID branding at startup

- [ ] 2.1 Extract a pure helper `fn ensure_aumid_branded(identifier: &str, current: AumidState) -> BrandingAction` that decides write/no-op from the current registry state (no I/O). Unit-test in isolation.
- [ ] 2.2 Write failing Rust tests for the helper (adversarial categories: idempotency, permission-denied non-fatal, first-launch both-values-missing): (a) both missing → `Write { display_name, icon_uri }`; (b) both present → `NoOp`; (c) the caller treats a registry-write error as non-fatal (warn-log + continue).
- [ ] 2.3 Make the tests pass by implementing the helper plus a thin Windows adapter that reads `HKCU\Software\Classes\AppUserModelId\<id>` and applies the action. The adapter is `#[cfg(target_os = "windows")]` and lives under an adapter module (not `domain/`).
- [ ] 2.4 Wire the adapter into app startup (`lib.rs` setup or a startup hook), running before any toast may be shown. Confirm the existing canonical `notifications` scenarios (consent gate, click-to-foreground) still pass.
- [ ] 2.5 Run `cargo test`; manually verify a `tauri dev` toast now displays (per the memory `project_dev_toast_aumid.md` recipe — `DisplayName` = "Meetily", `IconUri` = `file:///` URI to a bundled `.ico`).

## 3. B3 — Make the dev `meetily://` reactivation windowless (depends on §1)

- [ ] 3.1 Apply the §1 spike outcome: drop the `not(debug_assertions)` guard in `frontend/src-tauri/src/main.rs:1-4` (preferred), OR add the `AttachConsole(ATTACH_PARENT_PROCESS)` fallback, per the recorded decision.
- [ ] 3.2 Add a Rust doc/test note (or `#[cfg]` assertion) codifying that the binary is GUI-subsystem in both debug and release, so a future edit that re-introduces `not(debug_assertions)` is caught in review.
- [ ] 3.3 Re-run the §1.3 activation check on the final build; confirm no console flash in dev and no regression in release.

## 4. C3 — Conditional "View Meeting" action in the stop-completion toast

- [x] 4.1 Extract a pure helper `viewMeetingAction(meetingId: string | null): ToastAction | undefined` (returns the action object when `meetingId` is truthy, `undefined` otherwise). Per memory `feedback_hook_testing_extract_pure_helpers.md`, test the pure helper in Vitest without `renderHook` (no `@testing-library/react`).
- [x] 4.2 Write failing Vitest tests (adversarial categories: null, undefined, empty-string, valid-id) asserting: `null`/`undefined`/`''` → `undefined`; valid id → action with `label: 'View Meeting'` and an `onClick` that calls `router.push` with the id.
- [x] 4.3 Make the tests pass by implementing the helper, then wire `useRecordingStop.ts:167-182` to use it: `action: viewMeetingAction(meetingId)`.
- [x] 4.4 Remove the now-redundant `if (meetingId)` guard inside the `onClick` (the helper guarantees truthiness). Run `pnpm test` and `pnpm lint`.

## 5. Smoke spec (UI-affecting deliverable per CLAUDE.md §3)

- [ ] 5.1 Add `frontend/e2e/smoke/stop-notification-ux.spec.ts` covering the C3 wiring: emit a `recording-saved` flow with a known `meeting_id` and assert the "View Meeting" action appears and navigates; emit a flow with no `meeting_id` and assert no dead action renders. Use the event-bus mock seam (per memory `feedback_smoke_carveout.md`) — do not carve out as non-assertable without checking the wiring first.
- [ ] 5.2 Run `pnpm test:smoke` (kill any stale `pnpm dev` on :3118 first, per the local smoke gotcha). Confirm green.
- [ ] 5.3 Run the full pre-merge gate in parallel: `cargo test`, `pytest backend/`, `pnpm test`, `pnpm lint`, `pnpm test:smoke`. All must be green before `/opsx:archive`.

## 6. Archive readiness

- [ ] 6.1 Re-read `specs/notifications/spec.md`, `specs/recording-lifecycle/spec.md`, and `design.md`. Amend any delta whose implementation evolved during apply before archiving (per CLAUDE.md §3 gate).
- [ ] 6.2 Confirm branch is `fix/stop-notification-ux`, all commits reference the change name, and the pre-push hook passes (`SKIP_SMOKE=1` only if §5 is genuinely blocked — it should not be).
- [ ] 6.3 `/opsx:archive stop-notification-ux` after the §5.3 gate is green. Do NOT push or open a PR without explicit user authorization.
