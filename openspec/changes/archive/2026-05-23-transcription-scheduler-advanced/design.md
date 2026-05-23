## Context

`post-meeting-transcription` ships a scheduler with five AND-ed gates: `recording_active`, `meeting_detected`, `cpu_high (>70% / 30s)`, `ram_high (>80% / 30s)`, `manual_pause`. The thresholds are hardcoded; the gates always all apply. This is the conservative starting point — false-positive pauses are recoverable (resume button), greedy behaviour is the failure mode the user explicitly wanted to avoid.

Once the conservative defaults have shipped and been observed in practice, two refinements become useful: letting the user tune the thresholds, and letting the user choose between scheduling personalities ("just run it" / "be polite" / "I'll trigger manually"). This change adds those refinements.

The change is scoped to extending the existing scheduler — not redesigning it. The queue worker, pause-granularity behaviour, recovery flow, and IndexedDB persistence are untouched.

## Goals / Non-Goals

**Goals:**
- Expose the four numeric thresholds (CPU %, CPU duration, RAM %, RAM duration) through user-facing settings.
- Add a `scheduling_mode` preset that selects between gate combinations: `aggressive` disables CPU/RAM gates; `polite` (default) applies all gates with configured thresholds; `manual` disables auto-resume entirely.
- Settings live in the existing settings store and the existing settings UI conventions (no new architecture).
- The per-meeting `pauseReason` UI text reflects the active reason in human-readable form.
- Optionally add a GPU-load gate, only if `post-meeting-transcription` rollout shows CPU+RAM alone are insufficient.

**Non-Goals:**
- Calendar-aware scheduling (deferred).
- Per-meeting scheduling overrides (deferred).
- Cross-app priority hints (out of scope).
- Reworking the scheduler architecture (it's the existing `Scheduler` struct from `post-meeting-transcription`).

## Decisions

### D1: Three discrete modes, not a free-form policy editor

`aggressive | polite | manual` is a closed set. We avoid exposing a free-form "edit your own gate combination" because the space of useful combinations is small and the cognitive cost of presenting it is large.

- **`aggressive`**: gates that pause are `recording_active`, `meeting_detected`, `manual_pause`. CPU and RAM gates are disabled. Recommended for workstations or for users who don't share the CPU with other heavy tasks.
- **`polite`** (default): all five gates apply, with user-configured thresholds. The default values match what `post-meeting-transcription` ships.
- **`manual`**: no automatic resume after a job is enqueued. Each meeting requires a user click to start transcription. Whatever auto-stop conditions apply otherwise still apply mid-job (e.g., if the user starts a recording, the manual-running job still pauses). The user resumes by clicking "Run now" again.

Alternatives considered:
- A boolean per-gate ("enable CPU gate?", "enable RAM gate?"): more flexible but combinatorial and confusing. Rejected.
- A single "pause sensitivity" slider mapping to threshold combinations: nicer UX but hides what the gates actually do. Rejected — opacity is worse than precision here.

### D2: Threshold inputs are only visible in `polite` mode

`aggressive` and `manual` modes don't use the CPU/RAM thresholds. Showing them anyway invites confusion ("I set CPU to 30 % but transcription doesn't pause"). Hide them when the mode doesn't use them. The values still persist (so switching back to `polite` restores prior tuning).

### D3: Settings persistence in the existing settings store

The Tauri plugin-store (`scheduler_settings.json`) is used for persistence (matching the app's pattern for Rust-side settings). Add four numeric keys and one enum-string key:

| Key | Type | Default |
|---|---|---|
| `scheduling_mode` | string (`aggressive`/`polite`/`manual`) | `polite` |
| `cpu_pause_threshold_pct` | integer (1–100) | 70 |
| `cpu_pause_duration_secs` | integer (5–600) | 30 |
| `ram_pause_threshold_pct` | integer (1–100) | 80 |
| `ram_pause_duration_secs` | integer (5–600) | 30 |

The scheduler reads from settings on construction and re-reads on a `settings-changed` event. No hot-reload-per-sample needed (gates poll at 5 s intervals anyway).

### D4: GPU-load gate is conditional on rollout data

Adding a GPU-load gate sounds appealing — Whisper-on-Vulkan competes for GPU with the recording pipeline's RNNoise + EBU R128 + MP4 encode. But cross-platform GPU usage reading is gnarly (NVML for NVIDIA, ADL for AMD, IOKit on macOS), and we don't yet have evidence the CPU + RAM gates are insufficient in practice.

Decision: include the GPU gate in this change's *scope* but tag it as **tentative**, conditional on whether `post-meeting-transcription` rollout shows a real problem. If users report transcription degrading their recording quality despite the CPU/RAM gates, ship the GPU gate as part of this change. If not, drop it from this change's tasks and revisit later.

### D5: Per-meeting `pauseReason` text matches the active gate

`post-meeting-transcription` ships fixed reason strings (`recording_active`, `meeting_detected`, `cpu_high`, `ram_high`, `manual`). This change formats them for display with the current thresholds, e.g.:

- `Paused — you're recording` (constant)
- `Paused — you're in a meeting` (constant)
- `Paused — CPU above 70 % for 30 s` (uses configured threshold + duration)
- `Paused — RAM above 80 % for 30 s` (uses configured threshold + duration)
- `Paused — manually` (constant)

The user can therefore see exactly what triggered the pause and recognise whether their threshold setting is the cause.

## Risks / Trade-offs

- **[Risk]** Users set thresholds too aggressively (e.g., CPU 30 % / 5 s) and transcription never runs. → Mitigation: the per-meeting `pauseReason` text makes the cause obvious; the global queue indicator surfaces "N queued (paused — CPU above 30 % for 5 s)". The user sees what they set and can correct it. No protection against deliberate misconfiguration.

- **[Risk]** Users on `aggressive` mode degrade their own recording when M2 starts while M1 transcription runs. → Acceptable; the user opted into this mode, and `recording_active` still pauses (so M1 yields at chunk boundary anyway when M2 starts).

- **[Risk]** GPU gate adds platform-specific native dependencies. → Mitigation: only ship the gate if measurement justifies it; if shipped, gate it behind a Cargo feature and degrade gracefully when unavailable (treat as "GPU usage unknown, don't gate").

- **[Trade-off]** Three modes is fewer than the infinite combinations a per-gate boolean would allow. Acceptable — the modes are the policy categories that actually matter; everything else is mistuned configurability.

## Open Questions

- Should `manual` mode's "Run now" button be per-meeting or queue-wide ("run all pending now")? Recommend per-meeting; queue-wide can be added later if requested.
- Should the GPU gate's threshold be configurable on the same panel, or fixed if it ships? If we ship it, configurable (consistency with CPU/RAM). Decide at implementation time based on rollout data.
- Where should the migration of existing users' settings go? Settings persistence is keyed; absent keys read as defaults. No migration code needed.
