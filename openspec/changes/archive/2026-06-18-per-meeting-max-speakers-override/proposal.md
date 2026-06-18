## Why

Diarization structurally over-counts speakers on real meetings and there is no per-meeting way to correct it. The `max_speakers` cap is **global** — a single value in the `settings` table applied to every meeting identically. Seven independent counting methods (AHC, spectral eigengap, silhouette, BIC, ECAPA-TDNN, Sortformer embeddings, nemo_titanet-on-Sortformer-slots) all converge on 4 clusters for a known 3-speaker meeting; the over-count is structural in the audio, not an artifact of any single pipeline. The only durable fix that works regardless of pipeline is a user-facing cap — but the current global cap forces a user to either mis-tune every meeting or leave one meeting wrong. A per-meeting override lets the user say "this meeting had exactly 3 speakers" for the one meeting that's wrong, without touching the global default.

## What Changes

- Add a nullable `max_speakers INTEGER` column to the `meetings` table. `NULL` means "use the global default"; a value is a per-meeting override.
- The diarization cap resolution in `run_diarization_for_meeting` becomes: per-meeting override if set, otherwise the global `settings.max_speakers`. The most-isolated-cluster merging algorithm is unchanged.
- The cap remains an **upper bound**, not a target — diarization never splits a cluster to reach the configured number.
- Two new Tauri commands: `set_meeting_max_speakers(meeting_id, cap: Option<i32>)` (validates 2..=20, `None` clears the override) and `get_meeting_max_speakers(meeting_id)` returning both the override and the effective value.
- Frontend: a per-meeting "Max speakers" control in the meeting's speaker panel, with an explicit "Auto (use default)" option that maps to `NULL`. Setting the override stores it; the user applies it via the existing Re-diarize action (consistent with how re-diarization already clears labels).

## Capabilities

### New Capabilities
<!-- None — this extends the existing speaker-diarization capability. -->

### Modified Capabilities
- `speaker-diarization`: The `max_speakers` cap requirement changes from global-only to per-meeting-overridable. The cap is resolved per meeting (override if set, else global default); the most-isolated-cluster merging behavior and the [2, 20] range are unchanged.

## Impact

- **DB**: new migration adding nullable `meetings.max_speakers INTEGER`. No data backfill (all existing meetings default to `NULL` → global behavior, preserving current results).
- **Rust**: `frontend/src-tauri/src/audio/speaker/commands.rs` — cap resolution in `run_diarization_for_meeting` reads meeting override with global fallback; two new commands registered in `lib.rs`.
- **TypeScript**: `frontend/src/services/speakerService.ts` — `setMeetingMaxSpeakers` / `getMeetingMaxSpeakers`; a new control in the meeting speaker panel.
- **No new dependencies, no model changes, no API surface change** beyond the two Tauri commands. The global `settings.max_speakers` setting and its UI remain unchanged as the default.
- **Security**: `meeting_id` is already validated against the `meetings` table in the diarization path; the new commands reuse parameterized queries and the existing 2..=20 range check.
