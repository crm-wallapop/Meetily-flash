## Context

Three independent defects in the recording-stop / notification surface, all confirmed in-session:

| ID | Defect | Current code site |
|---|---|---|
| B2 | Dev-build AUMID registry key is empty → Windows drops toasts before display | `tauri.conf.json` identifier vs. `HKCU\Software\Classes\AppUserModelId\com.meetily.ai` |
| B3 | `meetily://` reactivation flashes a console window in dev | `main.rs:1-4` `windows_subsystem` debug-gated |
| C3 | "View Meeting" toast action renders unconditionally but its handler silently no-ops when `meetingId` is null | `hooks/useRecordingStop.ts:144,167-182` |

B3 is dev-only (release builds already use the GUI subsystem). B2/C3 affect any build where the code path is reached. None are data-layer, none touch ports/adapters, none require DB migration.

> **Scope history (2026-06-28):** A fourth defect, B1 (detection-time `recording_started` duplicate), was originally scoped in but verified already-fixed by commit `a08cc1d` (archived `notification-actions` change, 2026-06-23). `git blame` on `emit_detected` confirms the `recording_started` notification call was added in `ffc3bfa` and removed in `a08cc1d`; the current body emits only the in-app `meeting-detected` banner. No code change needed for B1.

## Goals / Non-Goals

**Goals:**
- B2: dev-build toasts display (AUMID branded at startup, idempotently, Windows-only).
- B3: no console/secondary window on `meetily://` reactivation in **both** debug and release builds.
- C3: the stop-completion toast's "View Meeting" action is conditional on a known `meetingId`; never a silent no-op.

**Non-Goals:**
- Detector accuracy (title-timing re-extract, meeting-lobby green-room FP) — deferred to a detector-discrimination change.
- Startup-migration-race fix — deferred (own change).
- `AudioCaptureBackend` clippy correctness lint — deferred (one-line cleanup, own change).
- Rewriting the notification manager, adding new notification types, or changing the consent-gate semantics.
- Touching the OS-notification spec's consent model or the recording-lifecycle phase machine.

## Decisions

### B2 — Idempotent AUMID branding at startup

At startup on Windows, the app reads `HKCU\Software\Classes\AppUserModelId\<identifier>`. If `DisplayName` or `IconUri` is missing, write both (`DisplayName` = "Meetily", `IconUri` = `file:///` URI to a bundled `.ico`). Idempotent: no-op when both are present. Non-fatal: registry failure logs a warning and continues. Dev-path only matters in practice (installed builds are expected to brand via the installer), but the startup check runs unconditionally and cheaply.

**Alternatives considered:**
- Require a Start Menu shortcut with the AUMID property (the classic Win32 toast recipe). Rejected: verified 2026-06-19 that just the two registry values suffice; a shortcut is unnecessary machinery.
- Brand only when the first toast is about to show (lazy). Rejected: by then the toast that triggered the branding is the one being dropped; branding must precede the first show.

### B3 — Drop the `not(debug_assertions)` guard on `windows_subsystem = "windows"`

`main.rs:1-4` currently applies the GUI subsystem only in release. Drop the `not(debug_assertions)` conjunct so the dev exe is also GUI-subsystem. Windows then allocates no console for the `meetily://` secondary, eliminating the flash. Logging in dev flows through the parent `tauri dev` terminal via inherited stdio handles.

**SPIKE (must precede the code change):** confirm `env_logger` still writes to the inherited parent terminal when the exe is GUI-subsystem. GUI-subsystem exes launched from a console keep the parent's stdout/stderr handles (already open, inherited), so this is expected to work — but verify by running `tauri dev` after the change and confirming `RUST_LOG=debug` output still appears in the launching terminal.

**Fallback if the spike fails:** call `AttachConsole(ATTACH_PARENT_PROCESS)` early in `main()` to re-attach the parent's console without allocating a new one, then proceed with logging as today. This preserves the dev terminal log experience without re-introducing the secondary-process console flash (the secondary exits before AttachConsole matters, or AttachConsole is a no-op for a process with no console).

**Alternatives considered:**
- Accept B3 as a dev-only cosmetic (release is correct). Rejected: the user observed it disrupting live debugging sessions; "release is fine" is not a fix.
- Register a separate GUI-subsystem helper exe as the protocol handler in dev, forwarding via single-instance IPC. Rejected: over-engineered for a dev-only cosmetic; requires building/shipping a second binary just for dev.
- `FreeConsole()` / `ShowWindow(SW_HIDE)` at the top of `main()` when the process detects it is the secondary. Rejected: detecting "I am secondary" requires duplicating the single-instance mutex logic before the plugin runs — fragile, races the plugin's own init.

### C3 — Conditional `action` render in the sonner toast

In `useRecordingStop.ts`, change the toast `action` to `meetingId ? { label: 'View Meeting', onClick } : undefined`. When `meetingId` is null, the toast shows the success message with no action; the sidebar refresh driven by `recording-saved-to-db` still surfaces the meeting for navigation. The existing `onClick` body (router.push + clearTranscripts + Analytics) is unchanged for the truthy path.

**Alternatives considered:**
- Defer the success toast until `recording-saved-to-db` delivers `meeting_id`. Rejected: delays positive feedback by the background_shutdown save latency (seconds); the user should see "Recording saved" immediately.
- Render the button always but show an inline "will appear in your sidebar shortly" toast when `meetingId` is null. Rejected: two toast variants to maintain for marginal benefit; omission is simpler and the sidebar is the canonical navigation surface.

## Risks / Trade-offs

- **[B3] Dropping the guard may suppress the dev console the user relies on for logs.** → Mitigation: the spike gates the change; if inherited stdio doesn't carry logs, fall back to `AttachConsole(ATTACH_PARENT_PROCESS)` which re-attaches the parent terminal without allocating a new console. Either way the dev terminal log experience is preserved.
- **[B3] Double-clicking the dev exe from Explorer shows no console.** → Mitigation: not a supported dev workflow (`tauri dev` is always launched from a terminal); document in CLAUDE.md platform notes. No user impact in release.
- **[B2] Registry write at startup may fail (permissions, AV).** → Mitigation: non-fatal; warn-log and continue. Installed builds are unaffected (installer brands the AUMID); dev builds fall back to the prior "toasts may drop" behavior, no worse than today.
- **[C3] Omitting the button when `meetingId` is null removes a navigation affordance.** → Mitigation: the sidebar refresh on `recording-saved-to-db` surfaces the meeting within seconds; the user is never blocked from navigating, just doesn't have a transient button for it.

## Migration Plan

No data migration, no API changes, no DB schema changes. Deploy is three code edits across `main.rs`, a new Windows AUMID helper, and `useRecordingStop.ts`. Rollback is `git revert` of the change's commits; no cleanup required (the B2 registry values are benign if left in place after rollback, and are already the documented dev-toast recipe).

## Open Questions

- **B3 spike outcome:** does `env_logger` write through inherited stdio under the GUI subsystem? Resolved in tasks.md §1 (spike task gates the B3 code change).
- **B2 installer behavior:** does Tauri's NSIS/MSI installer already brand the AUMID for release builds? If yes, the startup write is effectively dev-only (still harmless in release). Verify during apply; does not change the implementation either way.
