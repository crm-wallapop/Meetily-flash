## Why

Meetily's meeting detector is hardwired to Google Meet. The entry gate fuses three
signals inside one monolithic adapter (`detection/windows.rs`): a Meet-specific
window-title regex, a Google-CIDR TCP check (`has_meet_connection`), and a WASAPI
browser-capture check. Two structural problems follow:

1. **Generalization is blocked.** Adding Zoom, Teams, or Webex means editing the
   monolith — new CIDR lists, new title regexes, all entangled with Windows-specific
   window/WASAPI enumeration. There is no seam at which a second vendor can plug in.
2. **Title parsing is load-bearing for detection.** A two-codepoint dash bug
   (EM U+2014 vs EN U+2013) in the Meet title regex silently disabled **all** PWA
   detection — proof that a fragile title pattern is a single point of failure for the
   core "is a call happening?" signal. Title extraction and call detection should not
   fail together.

**Trade-off this change accepts:** dropping the title gate (below) arms the green-room
false positive for *all* Meet configurations — today the dash bug accidentally suppresses
it for the PWA only. The FP is **benign on the join path** (a few-seconds-early start of a
real call — the proposal's own data shows `getUserMedia` persists from green room into the
call; "benign" is relative, not absolute — the early start captures a few seconds of
**pre-join mic + system audio** before the user clicks *Join*. Meetily records audio only,
not camera frames, so the exposure is audio) and a **true spurious recording only on the
abandon path** (leave without joining).
A lobby discriminator is deferred to `meeting-lobby-discrimination`; see `design.md` D4
for the full FP analysis, including the F6 risk (Meet holding `getUserMedia` after
abandonment).

This change splits the detector along two hexagonal seams:

- **`CallSignalingPort`** — a vendor-neutral trait ("is there an active call-signaling
  connection?"). Each vendor ships an adapter carrying its own CIDR list; the detector
  ORs adapter results into the gate. The Meet adapter is the existing Google-CIDR check,
  extracted unchanged. New vendor = new adapter, no core change.
- **`MeetingTitleExtractorPort`** — a vendor-neutral trait ("what's the active call's
  window title, best-effort?"). Runs **after** detection as decoration. Returns
  `Option<String>`; `None` falls back to the existing generic-timestamp title. Detection
  never depends on it.

The gate drops the `has_title` conjunction: entry becomes `has_conn && not_preexisting`
where `has_conn = turn || (signaling_active && bc)`, with no title consulted. The Meet
title-extractor adapter carries the EN-dash fix that was the original scope of this
proposal — now safely demoted from "load-bearing regex" to "best-effort decorator."

**Why per-vendor CIDR adapters rather than a pure vendor-neutral gate:** a pure `bc + turn`
gate cannot detect TURN-less browser calls — including Meet's common UDP path. The grounding
is the WebRTC signaling model, not a single capture: signaling (HTTPS/WSS over TCP) runs
throughout a call for SDP exchange, ICE candidate trickle, and room state, independent of
the media transport — so a Meet UDP call keeps TCP connections to Google IPs even with no
TURN relay. The 2026-06-26 production sample confirms this for one call
(`has_meet_connection=true`, `has_browser_capture_session=true`, `turn=false` observed over
a 4 s in-call window); the model says it holds generally, with the caveat that TURN can be
negotiated mid-call under adverse network conditions. The Google-CIDR check is what catches
Meet-UDP calls. Dropping vendor knowledge entirely would regress the primary use case.
Relocating that knowledge behind a neutral port preserves coverage while making the
architecture genuinely generalize.

## What Changes

- **New port `CallSignalingPort`** (`ports/call_signaling.rs`):
  `fn is_call_signaling_active(&self) -> bool`. Pure trait, domain types only.
- **Meet signaling adapter** (`detection/signaling/meet.rs`): extracts the TCP-table scan
  (`has_meet_connection` + its `check_tcp4/6_connections` helpers) out of `windows.rs` into
  an adapter implementing `CallSignalingPort`. The Google CIDR constants already live in
  `detection/google_cidrs.rs` and are not moved.
- **New port `MeetingTitleExtractorPort`** (`ports/meeting_title_extractor.rs`):
  `fn extract_title(&self, windows: &[BrowserWindow]) -> Option<String>`. Pure trait.
- **Meet title-extractor adapter** (`detection/titles/meet.rs`): extracts the Meet title
  regex + `strip_google_meet_suffix` out of `windows.rs`. Carries the **EN-dash fix**
  (branch 3 + suffix splitter: `\u{2014}` → `\u{2013}`); unit-test fixtures corrected.
- **Detector gate drops `has_title`** (`use_cases/meeting_detection.rs`): entry is
  `has_conn && not_preexisting`, `has_conn = turn || (signaling_active && bc)`.
  `signaling_active` is the OR of wired `CallSignalingPort` adapters (v1: Meet only).
- **Title extraction wired post-detection**: on `MeetingDetected`, the use case calls
  `MeetingTitleExtractorPort::extract_title` over the enumerated windows to resolve
  `default_title`; `None` falls through to the generic-timestamp default.
- **Composition root** (`lib.rs`): wires the Meet signaling adapter and Meet title
  extractor into the detector / use case.

Non-goals: Zoom/Teams/Webex signaling or title adapters (no caller yet — the architecture
is additive; per YAGNI a second adapter and the `Vec<dyn CallSignalingPort>` aggregator
arrive together when a real second vendor lands, not before); native-app process-name
detection (Zoom.exe / Teams.exe — separate path, separate change); the green-room lobby
discriminator (still deferred to `meeting-lobby-discrimination` — and now **more** urgent,
see Risks in `design.md`).

## Capabilities

### New Capabilities

_(none)_

### Modified Capabilities

- `meeting-auto-detect`: the entry requirement loses the `has_title` conjunction (title no
  longer gates detection); two new requirements introduced — `CallSignalingPort` (the
  vendor-neutral signaling seam) and `MeetingTitleExtractorPort` (best-effort title
  decoration); the title-resolution requirement is re-sourced to the extractor port; the
  green-room FP Known-limitation is broadened from PWA-only to universal (no title gate
  suppresses it for any vendor), and a new cross-vendor CIDR+bc imprecision is documented.

## Impact

- **Code**:
  - `frontend/src-tauri/src/ports/call_signaling.rs`, `ports/meeting_title_extractor.rs` (new)
  - `frontend/src-tauri/src/detection/signaling/{mod,meet}.rs` (new — extracted from `windows.rs`)
  - `frontend/src-tauri/src/detection/titles/{mod,meet}.rs` (new — extracted from `windows.rs`, EN-dash fix)
  - `frontend/src-tauri/src/detection/windows.rs` (slimmed: loses `has_meet_connection`, the Meet
    title regex, `strip_google_meet_suffix`, and the Meet-title filter inside `enumerate_meet_windows`;
    keeps window enumeration + WASAPI + TURN)
  - `frontend/src-tauri/src/use_cases/meeting_detection.rs` (entry drops `has_title`; title-extraction call added)
  - `frontend/src-tauri/src/lib.rs` (composition-root wiring)
- **Spec**: `openspec/specs/meeting-auto-detect/spec.md` — entry requirement reworded (no `has_title`);
  two new requirements for the ports; FP Known-limitation broadened.
- **User-visible behavior**:
  - Meet PWA calls are detected (the dash bug no longer blocks detection — title isn't consulted for entry).
  - Meet recordings get correct titles (the extractor's EN-dash fix).
  - **Regression — green-room FP goes universal**: removing the title gate arms the green-room false
    positive for **all** Meet configurations (browser tab AND PWA). The title gate was the only thing
    accidentally suppressing it. Two paths: (a) **join** — the user clicks *Join now*; `getUserMedia`
    persists from green room into the call (per the verification data), so the "FP" is a
    few-seconds-early start of a real recording, not spurious; (b) **abandon** — the user leaves
    without joining; `bc` drops and the bc-drop exit fires `meeting-ended` (seconds), **unless** Meet
    holds `getUserMedia` open post-abandonment (the F6 idle-eviction analog), in which case the
    spurious recording runs until manual stop. Discriminator deferred to `meeting-lobby-discrimination`.
  - **New imprecision — cross-vendor CIDR+bc (unmeasured)**: a user with a Google TCP tab open (Gmail,
    Drive) AND a non-Meet browser call active can satisfy the Meet signaling adapter (`mc=true`) +
    `bc=true` and trigger a generic-tagged detection — a coincidence that is common in practice
    (pinned Gmail + any browser WebRTC call), not rare. CIDR+bc cannot prove the capture belongs to
    the CIDR's vendor. The title gate only partially mitigated this (it helped only when no
    Meet-titled window was present; a Meet tab alongside defeated it). v1 ships with the imprecision;
    severity is unknown until measured (`design.md` D4.2, Open Questions).
- **Sequencing**: `meeting-udp-confidence-debounce` was archived 2026-06-25; its exit/debounce edits
  to `detection/windows.rs` and `use_cases/meeting_detection.rs` have landed. This change rebases
  directly onto the post-debounce code — no in-flight conflict (`design.md` D6).
- **No breaking changes** to IPC contracts, storage, or frontend event shapes (`meeting-detected` /
  `meeting-ended` unchanged).
