## Why

Three independent user-visible defects live in the recording-stop / notification surface, all confirmed by the user in-session:

- **B2 — Dev builds silently drop OS toasts.** In `tauri dev` (uninstalled) builds the AUMID registry key (`com.meetily.ai`) exists but is empty (no `DisplayName` / `IconUri`), so Windows drops the toast before display even though `show()` returns `Ok`. Recipe verified 2026-06-19 but not yet wired into startup.
- **B3 — `meetily://` activation flashes a console window in dev.** `main.rs:1-4` gates `windows_subsystem = "windows"` on `not(debug_assertions)`, so the dev exe is console-subsystem. Every notification-action button re-launches the app via `meetily://`; the single-instance secondary that forwards the URI briefly owns an allocated console window. Release builds are already correct (GUI subsystem); this is dev-only but disrupts live debugging sessions.
- **C3 — "View Meeting" button in the stop-completion toast is a dead control.** `useRecordingStop.ts:144` computes `meetingId` from two nullable sources (`stopResult.meeting_id || activeMeetingId`); the toast at `:167-182` renders the action button *unconditionally*, but the `onClick` at `:174` silently no-ops when `meetingId` is falsy — visible button, zero feedback. This is the exact state where the transcription enqueue (`:149`) already failed.

All three are small, surgical, frontend/notification-surface fixes. Detector-accuracy items (title-timing, meeting-lobby green-room FP), the startup-migration-race, and the `AudioCaptureBackend` clippy correctness lint are explicitly **deferred** to their own changes — they are different layers and bundling them would violate the one-concern-per-change discipline.

> **Scope history (2026-06-28):** A fourth defect, B1 (detection-time `recording_started` duplicate), was originally included but verified already-fixed by commit `a08cc1d` (archived `notification-actions` change, 2026-06-23) — `emit_detected` now emits only the in-app `meeting-detected` banner, not the OS `recording_started` toast. Dropped from scope; no code change needed.

## What Changes

- **B2:** At app startup (dev path), populate the AUMID registry branding (`DisplayName` + `IconUri`) for the `tauri.conf.json` identifier so dev-build toasts are not silently dropped. Idempotent; no-op if already branded; reversible.
- **B3:** Stop the dev-build `meetily://` reactivation from flashing a console window. Preferred direction: drop the `not(debug_assertions)` guard on `windows_subsystem = "windows"` so the dev exe is GUI-subsystem and inherits the parent `tauri dev` terminal's stdio for logging. (Spike required: confirm `env_logger` still works via inherited handles under the GUI subsystem. Fallback directions documented in design.md.)
- **C3:** The recording-stop completion toast's "View Meeting" action SHALL be rendered conditionally on a known `meetingId`; when `meetingId` is unknown the action SHALL be omitted (or replaced with an explicit "will appear in your sidebar" message), never a silent no-op.

No **BREAKING** changes. All fixes are behavior-preserving for the happy path; they only remove defects.

## Capabilities

### New Capabilities

_(None.)_ All affected behavior is already governed by existing canonical specs.

### Modified Capabilities

- `notifications`: B2 (dev-build AUMID branding so toasts actually display), B3 (`meetily://` reactivation must be windowless — the reactivation path is the one notification action buttons use).
- `recording-lifecycle`: C3 (the post-stop completion affordance — an in-app sonner toast, not an OS toast — must navigate or be omitted, never silently no-op).

## Impact

- **Rust (`frontend/src-tauri/src/`)**: `main.rs` (B3 subsystem attribute), a new startup AUMID-branding helper for Windows (B2). All Windows-gated where relevant.
- **TypeScript (`frontend/src/`)**: `hooks/useRecordingStop.ts` (C3 conditional action render). No new types; no adapter or port changes.
- **No port/adapter changes** (hexagonal boundaries unchanged). No DB migrations. No new dependencies.
- **Adversarial test surface (CLAUDE.md §4)**: AUMID-branding idempotency + reversal (B2), GUI-subsystem log inheritance (B3 spike), null-`meetingId` toast rendering (C3). Smoke spec: `frontend/e2e/smoke/stop-notification-ux.spec.ts` (per the UI-affecting smoke deliverable rule).
