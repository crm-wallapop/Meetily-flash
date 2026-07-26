# Exploration: Meeting Detection / Lobby Discrimination in Google Meet PWA

> **Status:** Exploration findings (explore mode). Not a proposal yet.
> **Date:** 2026-06-25
> **Context:** `/opsx:explore "Meeting detection/abandonment in Google Meet desktop app"`
> **Capability:** `openspec/specs/meeting-auto-detect/spec.md`

> **Re-evaluation (2026-07-01, explore mode):** The framing of this doc — "the dash
> fix MUST ship with a lobby discriminator (F5)" — is **historically superseded.**
> `vendor-neutral-meeting-detection` landed on main (2026-06-30): it dropped `has_title`
> from the gate entirely and shipped the EN-dash fix inside the Meet title-extractor
> adapter, *without* a discriminator. Post-landing, the canonical spec reframes the
> join-path green-room FP as a **benign few-seconds-early start** (Meetily records audio,
> not camera). What remains is only the **abandon-path** junk artifact — dismissable, not
> data loss.
>
> **Verdict: a discriminator is NOT worth building on current evidence.** Every candidate
> (B: duration-based discard; C: render-energy retraction; D: render-Active/TURN entry
> gate) trades junk-recording reduction for **false-negatives on real calls** — an
> upside-down trade for a recorder (a junk recording costs a click; a missed real call
> costs the meeting). B is weak: the abandon-FP duration is unbounded (green-room dwell),
> so it either catches almost nothing (safe tiny-N) or silently deletes legit short calls
> (aggressive-N). C catches the long tail + F6 but (a) adds a silent-real-call loss class,
> (b) its cheap signal (whole-device render energy) is defeated by ambient audio
> (Spotify/YouTube) while its robust signal (per-browser-process render energy) is a new
> WASAPI subsystem, and (c) needs a Meet-only title-code composite to close the solo-call
> gap. D is silent data loss at entry.
>
> **Dispositions for the residual concerns:**
> - Junk accumulation → optional tiny-N (≤10s) silent discard of detector-started
>   recordings on `meeting-ended`, as a back-pocket follow-up; only if observed. No new
>   detection state; reuses `cancel_recording` (`lib.rs:262`).
> - **F6** (getUserMedia held after Meet idle-eviction) → **NOT worth a change** (decided
>   2026-07-02): Meet's auto-kick ends the real meeting, so F6 only appends post-call junk
>   — dismissable, not data loss; every fix trades false-negative risk on real long calls
>   (same trade as the discriminator). bc-persistence was never measured; revisit only if a
>   runaway is observed.
> - **Cross-vendor CIDR+bc FP** → none of B/C/D touch it; the canonical spec's §8 task to
>   *measure* its frequency is the right gate. Re-open the discriminator question ONLY if
>   that measurement shows FPs are routine.

## TL;DR

The Meet **PWA** title-format dash bug and the **green-room false-positive** are not
independent problems — they are **the same bug viewed from two sides**. The regex
expects an EM dash (U+2014) where Meet emits an EN dash (U+2013). That mismatch
currently prevents *both* (a) correct in-call detection and (b) a green-room false
positive. Fixing the dash without also adding a lobby discriminator would **introduce**
the false positive. They must be solved together.

---

## Findings

### F1 — PWA title uses EN dash; regex expects EM dash

Empirically observed (PID 24408, `msedge`, Meet PWA):

| State | Window title | Dash at "Meet <dash> <id>" |
|---|---|---|
| In-call | `Google Meet - Meet – opv-augt-jbm` | **U+2013 (EN DASH)** |
| Green room | `Google Meet - Meet – 🎡 Search XP Playground (new)` | **U+2013 (EN DASH)** |
| Landing / left | `Google Meet` | (none) |

Production regex branch 3 (`frontend/src-tauri/src/detection/windows.rs:103`):

```regex
^Google Meet - Meet \u{2014} .+      // EM DASH — never matches the PWA
```

Second site: `windows.rs:81` `strip_google_meet_suffix` splits on `\u{2014}` (EM).
The unit test at `windows.rs:897-903` (`title_parsing_pwa_format`) encodes the bug —
its fixtures use EM dash, so the test passes while production titles (EN dash) never match.

### F2 — The dash bug and the lobby FP are coupled (load-bearing bug)

The green-room title format is **byte-identical in structure** to the in-call title:

```
Google Meet - Meet – <identifier>      // EN dash (U+2013) in both states
```

Therefore:

| State | Branch 3 matches (EM dash)? | Effect of current bug |
|---|---|---|
| In-call | NO (EN ≠ EM) | Detection **fails** — the reported "PWA not detected" bug |
| Green room | NO (EN ≠ EM) | FP **accidentally prevented** |

**Implication:** changing branch 3 from `\u{2014}` → `\u{2013}` fixes in-call detection
**and** simultaneously arms the green-room false positive. The dash bug is load-bearing —
it is currently the only thing standing between a working PWA title match and a lobby FP.
Any proposal that fixes F1 MUST also solve the lobby discrimination problem (F5).

### F3 — Entry logic does not consult `has_browser_capture_session`

`frontend/src-tauri/src/use_cases/meeting_detection.rs:94`:

```rust
if has_title && has_conn && not_preexisting {   // has_conn = observation.has_meet_connection
```

> **Correction (2026-06-26):** the original draft of this section claimed bc has "zero
> influence on the entry decision." That was wrong. The adapter (`detection/windows.rs:761-
> 776`) computes `has_conn = if turn { true } else { mc && bc }` and stores it in
> `observation.has_meet_connection` (line 818). So bc **is** part of the entry condition —
> it is AND'd with `mc` inside the combined `has_conn` field, not checked separately by the
> state machine. The spec confirms this at line 101: *"The `has_meet_connection` observation
> SHALL equal `has_turn_connection() || (has_meet_connection() && has_browser_capture_session())`."*
>
> The conclusion below is unchanged, because the green room has `bc=true` (getUserMedia
> camera preview) regardless — so `has_conn = mc && bc = true && true = true` for the green
> room. bc being in the entry condition does not help discriminate.

~~`has_browser_capture_session` (bc, WASAPI eCapture Active) appears **only** in the InCall
branch (`meeting_detection.rs:124`), where it clears the exit debounce to keep a call
alive. It has **zero influence on the entry decision**.~~ (Stricken — see correction above.)

Consequence: any WASAPI-based lobby discriminator would still need to be a **new entry
gate** — not because bc is absent from entry (it isn't), but because bc reads `true` in
both the green room and in-call, so it cannot discriminate them. eRender Active is the
untested candidate (see F5).

### F4 — Spec error: green room DOES have a Google TCP connection

`openspec/specs/meeting-auto-detect/spec.md:16-18` describes the "Meet tab open but user
has not joined" scenario as having no Google connection. This is **empirically wrong**.

Observed during green room (pre-join camera preview):

```
msedge → 142.251.36.202:443   (142.250.0.0/15 Google range)
```

The green room establishes HTTPS / WebSocket signaling to Google IPs, so
`has_meet_connection` returns **true**. Combined with F2 (title matches once dash is
fixed) and `not_preexisting` (true if Meetily started before navigation), **all three
entry signals are satisfied in the green room** — the FP is real once F1 is fixed.

### F5 — No current signal cleanly discriminates green room from in-call (PWA)

| Signal | Green room | In-call | Discriminates? |
|---|---|---|---|
| Title format | identical (`Google Meet - Meet – <id>`) | identical | **no** |
| `has_meet_connection` (Google TCP) | true | true | **no** |
| `has_browser_capture_session` (eCapture) | true (getUserMedia) | true | **no** |
| `has_turn_connection` (TURN TCP) | **false?** (unverified) | true — TURN calls only | **partial**: UDP calls don't use TURN, so requiring TURN at entry would miss them |
| eRender Active (incoming audio) | **false?** (unverified) | true — if others unmuted | **partial**: fails for solo / all-muted meetings |

The only two candidates (TURN-at-entry, render-at-entry) are **imperfect** — both have
legitimate in-call states where they read false. An entry debounce does not help either:
the green room can persist indefinitely while the user adjusts camera/mic settings.

### F6 — Stale detection after Meet idle-eviction (new failure mode)

Observed: after Meet disconnects an idle solo call ("meeting expired because you weren't
saying anything"), the tab **holds `getUserMedia` open**. The title may still match the
in-call regex and `bc=true` persists, so the detector stays InCall indefinitely.

This is a **third** failure mode, not documented in the spec. It is distinct from the
lobby FP (F2/F4) and from the exit debounce logic (which assumes bc eventually drops).

### F7 — InCall title-blindness (rapid meeting switching merges recordings)

The state machine reads `default_title` **only** at the Idle→InCall transition
(`meeting_detection.rs:95`). During InCall, the title is ignored entirely. If a user
leaves meeting A and joins meeting B within a single debounce window (so the detector
never transitions through Idle), the two meetings merge into one recording with A's title.

This is the "rapid meeting switching" thread. It is orthogonal to the lobby FP but
compounds the discrimination problem: even if we solve entry, mid-call title changes are
invisible to the detector.

---

## Empirical data captured

### WASAPI session counts (single long-lived MTA thread, GetDisplayName-based)

| State | Title | CAP (eCapture) Active | REN (eRender) Active |
|---|---|---|---|
| In-call | `Google Meet - Meet – opv-augt-jbm` | 1 | 1 |
| Left / landing | `Google Meet` | 0 | 0 |
| Green room | `Google Meet - Meet – 🎡 …` | **not sampled** | **not sampled** |

The green-room WASAPI sample was **not captured** — the user stepped away before the
looping diagnostic ran. This is the most important missing data point for F5's render
hypothesis. Approach to recreate: see "Diagnostic recipes" below.

### Entry logic trace (from reading `meeting_detection.rs`)

```
Idle ──[ has_title && has_meet_connection && not_preexisting ]──▶ InCall
                                                                     │
                                                     bc=true (every poll) clears exit timer
                                                     bc=false for ≥ debounce ──▶ Idle + MeetingEnded
```

bc (eCapture) is a **stay-alive** signal, not an entry signal. TURN calls exit on a 4 s
debounce (`is_turn_exit`); UDP calls exit on 4 s (stable mic) or 15 s (transient-prone).

---

## Open questions (ranked by leverage)

1. **Green-room WASAPI render sample** — does eRender go Active=0 in the green room while
   eCapture stays Active=1? If yes, render-at-entry is a viable (if imperfect)
   discriminator. This is the cheapest test with the highest information value.
2. **Green-room TURN connections** — does `has_turn_connection` read false in the green
   room? If yes, TURN-at-entry discriminates for TURN calls (but not UDP calls).
3. **Non-PWA browser tab title** — does a regular Chrome/Edge tab (not PWA) show a
   different green-room title format? The PWA prepends "Google Meet - "; a plain tab may
   not, which would mean branches 1/2/4 apply differently.
4. **Entry debounce feasibility** — is there any OS-level signal that transitions
   green-room → in-call that the detector could latch onto (e.g., TURN appearing, render
   going Active) to fire a *delayed* confirmation?
5. **Idle-eviction teardown timing** — how long does getUserMedia persist after Meet
   idle-evicts? Does the title eventually change, or stay frozen?

---

## Diagnostic recipes (scratchpad scripts are ephemeral — recreate from these)

All scripts lived under the session scratchpad and will be cleaned up. The approaches:

### WASAPI render/capture session counter (PowerShell + raw COM vtable)

Enumerates `eCapture` and `eRender` endpoints via `IMMDeviceEnumerator`, activates
`IAudioSessionManager2`, counts sessions with `GetState() == AudioSessionStateActive (1)`.

**Key implementation notes (learned the hard way):**
- .NET RCW `ComImport` casts fail with `E_NOINTERFACE` in PowerShell — use **raw vtable
  calls** via `[UnmanagedFunctionPointer(CallingConvention.StdCall)]` delegates +
  `Marshal.ReadIntPtr(vtbl, slot * IntPtr.Size)`.
- `IAudioSessionControl2` QI fails (`E_NOINTERFACE`) from the raw session pointer —
  **don't rely on `GetProcessId`** for browser-session identification. Use
  `GetDisplayName` (vtable slot 4 on `IAudioSessionControl`, no QI needed) instead.
  Display names are often empty for browser sessions, but the **Active count pattern** is
  what matters.
- Run on a **single long-lived MTA thread** (`CoInitializeEx(COINIT_MULTITHREADED)` once,
  loop internally). Spawning a new MTA thread per sample tears down COM on exit and
  corrupts the next call's `CoCreateInstance` (returns -1).
- Vtable slots used: `Release`=2, `EnumAudioEndpoints`=3, `GetCount`=3, `Item`=4,
  `Activate`=3, `GetId`=5, `GetSessionEnumerator`=5, `GetSession`=4, `GetState`=3,
  `GetDisplayName`=4.

### Title codepoint dumper (PowerShell + Get-Process)

`Get-Process chrome,msedge,... | MainWindowTitle`, then iterate `ToCharArray()` printing
`[i] = U+XXXX (name)` for any char with `cp > 127` or in the dash family (U+002D,
U+2010–U+2015). Tests against all 4 regex branches with explicit dash literals.

**Gotcha:** inline PowerShell via Bash mangles `$_.Property` (bash expands `$_`). Put the
script in a `.ps1` file and run with `-File`, never `-Command`.

---

## Spec corrections needed (when formalizing)

These should be folded into any proposal that touches `meeting-auto-detect`:

1. **spec.md:16-18** — "Meet tab open but user has not joined" claims no Google
   connection. Correct: the green room **does** have Google TCP (signaling).
2. **spec.md:61-63** — exit-lobby scenario correctly notes getUserMedia is active; this
   should be cross-referenced with the entry-side risk (F4).
3. **spec.md:143-145** — "Known limitation" documents only the lobby-FP *suppression*
   flavor (app-start `not_preexisting` guard). The *false-positive* flavor (navigate to
   green room while Meetily runs) is undocumented and is the active risk once F1 is fixed.
4. **windows.rs test at :897-903** — fixtures use EM dash; must switch to EN dash AND
   add green-room-title fixtures once a discriminator exists.

---

## Candidate change names (for when this becomes a proposal)

- `vendor-neutral-meeting-detection` — (was `fix-meet-pwa-title-dash`; renamed+rescoped
  2026-06-26). Two-port hexagonal refactor that drops `has_title` from the gate and ships
  the EN-dash fix inside a Meet title-extractor adapter. The F2 coupling is dissolved
  (title no longer gates) but the green-room FP goes **universal** as a result — see
  `openspec/changes/vendor-neutral-meeting-detection/design.md` D4.
- `meeting-lobby-discrimination` — the discriminator (F5). **NOT RECOMMENDED on current
  evidence** (2026-07-01 re-evaluation above): the target is a dismissable artifact, not
  data loss, and every mechanism trades junk-reduction for false-negatives on real calls.
  Re-open only if the §8 cross-vendor-FP frequency measurement shows FPs are routine.
- `meeting-stale-idle-eviction` — F6. **NOT worth a change** (decided 2026-07-02): Meet's
  auto-kick ends the real meeting, so the runaway only appends post-call junk (dismissable,
  not data loss); every candidate fix trades false-negative risk on real long calls.
  bc-persistence was never measured; revisit only if a runaway is observed in practice.
- `meeting-incall-title-tracking` — F7, likely orthogonal.

**Ordering (revised 2026-07-02):** F1 already landed via vendor-neutral. F5 (discriminator)
and F6 (idle-eviction) are both **parked — not worth changes on current evidence**, each
with a documented re-open trigger (§8 FP frequency for F5; observed runaway for F6). F7 can
proceed independently if it bites.
