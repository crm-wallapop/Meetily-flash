## Why

`post-meeting-transcription` ships a transcription scheduler with hardcoded thresholds (CPU 70 % over 30 s, RAM 80 % over 30 s) and a single pause-policy that always applies. These defaults are conservative and work for the typical case, but they may be wrong for two distinct user populations: power users who want transcription to start sooner (e.g., on a workstation where 70 % is normal idle), and users on constrained hardware (e.g., a low-RAM laptop where 80 % is reached during everyday browsing). This change exposes the scheduler's behaviour through user-facing settings so each user can tune trade-offs.

It also introduces a scheduling-mode preset (`aggressive | polite | manual`) so users don't have to understand the individual gates to make a sensible choice.

## What Changes

- Add a new `Advanced > Background processing` section in the settings UI containing:
  - **Scheduling mode** preset: `aggressive` / `polite` (default) / `manual`
  - **CPU pause threshold** (%, default 70) — visible only in `polite` mode
  - **CPU sustained duration** (seconds, default 30) — visible only in `polite` mode
  - **RAM pause threshold** (%, default 80) — visible only in `polite` mode
  - **RAM sustained duration** (seconds, default 30) — visible only in `polite` mode
- `aggressive` mode disables the CPU and RAM gates; only `recording_active`, `meeting_detected`, and `manual_pause` still pause work. Useful on workstations.
- `manual` mode disables all auto-resume; the worker only runs when the user clicks "Run now" per meeting. Useful when the user wants full control.
- `polite` mode is the current default and applies all gates with the configured thresholds.
- Persist settings via the Tauri plugin-store (`scheduler_settings.json`).
- Add the per-meeting `pauseReason` UI badge with full text (e.g., `Paused — CPU above 70 % for 30 s`) so users understand why a job is paused.
- Add a GPU-load gate as an additional `polite`-mode signal, conditioned on availability of a cross-platform GPU usage reader. **Status: tentative — only ship if measurement during `post-meeting-transcription` rollout shows CPU+RAM alone are insufficient.**

## Capabilities

### New Capabilities
- (none)

### Modified Capabilities
- `post-meeting-pipeline`: Scheduler thresholds become user-configurable; mode preset added; gate set may grow to include GPU load.

## Impact

- `frontend/src-tauri/src/use_cases/transcription_queue.rs` — scheduler reads thresholds and mode from settings instead of constants
- `frontend/src-tauri/src/use_cases/scheduler_settings.rs` — new: `SchedulerSettings`, `SchedulerLiveConfig`, load/save via Tauri plugin-store
- `frontend/src/components/BackgroundProcessingSettings.tsx` — new segmented control for scheduling mode + threshold inputs
- `frontend/src/services/schedulerSettingsService.ts` — new adapter for Tauri invoke calls
- `frontend/src/hooks/useQueueJobStatus.ts` — hooks for queue snapshot, progress, settings, and label formatting
- `frontend/src/components/QueueStatusBadge/QueueStatusBadge.tsx` — per-meeting status pill with "Run now" button
- Optional: cross-platform GPU usage reader (e.g., behind a feature gate or a native dependency); decision deferred to design phase based on measurement
- **Prerequisite**: `post-meeting-transcription` must be applied and archived first; this change extends its scheduler

## Out of Scope

- Per-meeting scheduling overrides (e.g., "always run transcription for this meeting immediately regardless of gates"). Defer until requested.
- Calendar-aware scheduling (look ahead at upcoming meetings to defer transcription). Heavy integration; revisit if/when calendar features land.
- Cross-application priority hints (e.g., yield to a specific other process). Operating-system-specific; out of scope.
