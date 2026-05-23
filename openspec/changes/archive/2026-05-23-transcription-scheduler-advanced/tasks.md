## 1. Settings store

- [x] 1.1 Add new keys to `scheduler_settings.rs` (Tauri plugin-store): `scheduling_mode` (string, default `"polite"`), `cpu_pause_threshold_pct` (i32, default 70), `cpu_pause_duration_secs` (i32, default 30), `ram_pause_threshold_pct` (i32, default 80), `ram_pause_duration_secs` (i32, default 30)
- [x] 1.2 Write a test asserting absent keys return their documented defaults
- [x] 1.3 Extend the `get_settings` / `save_settings` Tauri commands to include the new keys; TypeScript types updated in `frontend/src/services/`

## 2. Scheduler reads from settings

- [x] 2.1 Write a failing test `scheduler_reads_thresholds_from_settings`: with settings set to CPU=40%/15s, mock CPU at 45% sustained 15s, assert the gate becomes busy (instead of waiting for 70%/30s)
- [x] 2.2 Write a failing test `scheduler_aggressive_mode_disables_cpu_and_ram_gates`: with `scheduling_mode = "aggressive"`, mock CPU at 99% sustained and RAM at 99% sustained, assert both gates report clear
- [x] 2.3 Write a failing test `scheduler_manual_mode_does_not_auto_resume`: with `scheduling_mode = "manual"` and all gates clear, assert the worker does NOT auto-pick up `pending` jobs; only runs jobs explicitly invoked via a "Run now" command
- [x] 2.4 Refactor the `Scheduler` struct to take thresholds and mode from settings (instead of constants); add `mode: SchedulingMode` enum
- [x] 2.5 Add a `settings-changed` listener so the scheduler hot-reloads its config without app restart
- [x] 2.6 Run tests 2.1–2.3 green

## 3. Manual mode and "Run now" command

- [x] 3.1 Add Tauri command `run_transcription_job_now(meeting_id)` that bypasses the auto-resume check for that specific job
- [x] 3.2 Write a test asserting `run_transcription_job_now` triggers the worker even in `manual` mode (subject to the other gates: still pauses if recording is active)
- [x] 3.3 Run test 3.2 green

## 4. Settings UI panel

- [x] 4.1 Add a new `Advanced > Background processing` section in the Settings component
- [x] 4.2 Render the mode selector (radio group: `aggressive`, `polite`, `manual`) with descriptive helper text under each
- [x] 4.3 Conditionally render the four threshold/duration numeric inputs only when mode is `polite`
- [x] 4.4 Validate ranges client-side: CPU/RAM 1–100, durations 5–600; show inline error on invalid input; persist only on valid input
- [x] 4.5 Vitest tests for the panel: render-mode-switching, validation, persistence wiring

## 5. Per-meeting pause-reason UI

- [x] 5.1 Update the per-meeting queue badge to format `pauseReason` with current thresholds:
  - `recording_active` → "Paused — you're recording"
  - `meeting_detected` → "Paused — you're in a meeting"
  - `cpu_high` → `Paused — CPU above {cpu_pause_threshold_pct} % for {cpu_pause_duration_secs} s`
  - `ram_high` → `Paused — RAM above {ram_pause_threshold_pct} % for {ram_pause_duration_secs} s`
  - `manual` → "Paused — manually"
- [x] 5.2 Vitest tests for each variant

## 6. GPU gate (tentative — only if rollout data justifies)

- [ ] 6.1 Gather rollout data from `post-meeting-transcription` for at least one week of use; check whether transcription degrades recording quality despite CPU+RAM gates
- [ ] 6.2 **Decision point:** if degradation observed, proceed; if not, mark §6 as dropped and update the proposal to remove the GPU gate
- [ ] 6.3 (Conditional) Add GPU usage reader behind a `gpu_telemetry` Cargo feature; degrade to "unknown, don't gate" when feature disabled or platform-unsupported
- [ ] 6.4 (Conditional) Add `gpu_pause_threshold_pct` and `gpu_pause_duration_secs` to settings (defaults 60%/30s); wire into scheduler when in `polite` mode
- [ ] 6.5 (Conditional) Add UI controls + tests

## 7. Verification

- [x] 7.1 Run full suite: `cargo test`, `pytest backend/`, `pnpm test`, `pnpm lint` — all green
- [x] 7.2 Manual smoke: set mode to `aggressive` → confirm CPU/RAM gates have no effect; jobs run regardless
- [x] 7.3 Manual smoke: set mode to `polite` with tight thresholds (CPU 30%/10s) → confirm jobs pause when normal app activity pushes CPU over 30%
- [x] 7.4 Manual smoke: set mode to `manual` → confirm jobs stay `pending` until "Run now" is clicked; once running, still respect `recording_active` pause; chains Transcribing → Summarising → Done without re-clicking
- [x] 7.5 Verify segmented control arrow-key navigation and settings hot-reload

## 8. Spec drift and archive prep

- [x] 8.1 Re-read this proposal, design, and the delta spec; confirm implementation matches every scenario
- [x] 8.2 Update the delta on `post-meeting-pipeline` to reflect any new requirements added in §6 if the GPU gate ships
