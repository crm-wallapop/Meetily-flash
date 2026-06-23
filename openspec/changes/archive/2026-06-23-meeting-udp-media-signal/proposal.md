## Why

UDP-transport meeting exit takes ~16–19 s today (15 s debounce + ~1–2 s `getUserMedia`
release + poll granularity). The 15 s debounce exists because the exit signal
(`has_browser_capture_session`, "bc") transiently drops up to ~10 s mid-call on a
device switch, so a fixed 15 s covers that worst case — yet most calls never have
such a transient, so every call pays the full tax for a rare event.

The canonical spec's own proposed remedy (`openspec/specs/meeting-auto-detect/spec.md`
line 107: a future `GetExtendedUdpTable`-to-Google-IPs check) is **impossible**:
`GetExtendedUdpTable` exposes local addr + local port + owning PID only — no remote
address (UDP is connectionless) — so the WebRTC media flow cannot be distinguished
from QUIC by that API. The clean alternatives are blocked: ETW/WFP flow capture
needs admin privileges, and CDP needs the browser launched with a debug flag. The
one cheap, unblocked lever is to make the exit debounce **adaptive** to the call's
observed mic stability.

## What Changes

- **Adaptive UDP exit debounce.** The adapter tracks whether `bc` has dropped at any
  point during the current call. If `bc` has been stable (no drop observed), exit uses
  a short debounce (~4 s); if a drop was ever observed this call, the existing long
  debounce (15 s) applies. Stable-mic calls (the common case) exit in ~5–6 s instead
  of ~16–19 s; transient-prone setups keep the safe 15 s.
- **New observation field `stable_capture`** on `DetectorObservation` (port type),
  mirroring the existing `is_turn_exit` plumbing: set by the adapter from per-call
  bc-drop history, consumed by the pure `step_detector` to select the UDP debounce
  duration. Reset to the conservative default by `notify_exit()`.
- **Correct the canonical spec's line-107 known-limitation note**, which claims a
  future `GetExtendedUdpTable`-to-Google-IPs check would discriminate the lobby from
  an active call. That is impossible with the named API. The note is replaced with an
  accurate statement that the UDP-media-flow direction is abandoned (with the reason)
  and that adaptive debounce is the adopted lever.
- The fixed 15 s UDP debounce value is replaced by the adaptive selection in the exit
  requirement. The 4 s TURN debounce is unchanged.

## Capabilities

### New Capabilities
<!-- None -->

### Modified Capabilities
- `meeting-auto-detect`: the "Detect when an active call ends" requirement's UDP-path
  debounce becomes adaptive (short when the call's bc signal was stable, long when a bc
  drop was observed), and the known-limitation note about a future UDP-socket check is
  corrected to reflect that the named API cannot do what it claims.

## Impact

- **Code**: `frontend/src-tauri/src/detection/windows.rs` (track per-call bc drops;
  set `stable_capture`; reset on `notify_exit`); `frontend/src-tauri/src/ports/meeting_detector.rs`
  (add `stable_capture: bool` to `DetectorObservation`); `frontend/src-tauri/src/use_cases/meeting_detection.rs`
  (`step_detector` InCall branch selects UDP debounce from `stable_capture`);
  `frontend/src-tauri/src/detection/fake.rs` + the `dev-detector` seam (populate the new
  field; default `false` = conservative long debounce). No change to trait method
  signatures.
- **Spec**: `openspec/specs/meeting-auto-detect/spec.md` — exit requirement (adaptive
  debounce) and the line-107 known-limitation note corrected.
- **Tests**: adversarial — stable call exits on the short debounce; a bc drop observed
  mid-call forces the long debounce for the rest of that call; the residual first-drop
  false-exit risk on a previously-stable call (accepted trade-off, mitigated by the
  existing auto-stop "signals re-engage → prompt dismissed silently" behavior).
- **User-visible**: meeting-end detected ~10–13 s sooner on the common stable-mic path;
  no regression for transient-prone setups.
- **Risk**: low–medium. The adaptive signal is a per-call heuristic; worst case degrades
  to today's 15 s. No privilege change, no new native dependencies.
