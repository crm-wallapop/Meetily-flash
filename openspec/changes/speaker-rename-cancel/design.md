## Context

The speaker rename flow:

```
SpeakerBadge (display)  ──click──▶  setEditingSegmentId(transcript.id)
                                         │
TranscriptView.tsx:324  editingSegmentId === transcript.id
                                         │
                                         ▼
                              SpeakerLabelInput
                              ├─ autoFocus (line 120)
                              ├─ onKeyDown: Enter→onSubmit, Escape→onCancel (103–109)
                              ├─ suggestion chips → onSubmit(name) (134–142)
                              └─ ** no onBlur **
```

`onSubmit` → `useSpeakerRename.handleSpeakerSubmit` → `labelSpeaker` (cluster
rename). `onCancel` → `setEditingSegmentId(null)` → input unmounts.

The input mounts via `autoFocus` and never relinquishes focus unless the user
presses Escape or Enter. Clicking elsewhere in the document leaves it focused
and visible — the "trapped open" symptom.

### Why now: the suggestion-chip race

A naive `onBlur={onCancel}` regresses suggestion selection. The chips are
`<button>`s rendered as siblings of the `<input>` inside the same container
(`SpeakerBadge.tsx:134`). `mousedown` on a button moves focus out of the input
→ `onBlur` fires synchronously → `onCancel` runs → `setEditingSegmentId(null)`
→ the whole `SpeakerLabelInput` subtree unmounts → the chip's subsequent
`click`/`onSubmit` never fires. The user clicks "Alice", nothing happens, the
input closes. This is the standard React inline-edit pitfall and is why the fix
is two lines, not one.

## Goals

- Click-outside the open input cancels without committing (matches Escape).
- Suggestion-chip selection still submits the selected name (no regression).
- No change to Enter (submit) or Escape (cancel) semantics.

## Non-Goals

- Submit-on-blur when the field is non-empty (alternative UX; explicitly
  rejected — see D1).
- Per-turn override scope toggle (separate change `per-turn-speaker-override`).
- Color resolution for named labels (pre-existing latent issue in
  `TranscriptView.tsx:333` `colorIndex` parse; out of scope).

## Design

### D1 — Cancel-on-blur, not submit-on-blur

Two common inline-edit conventions:

| Behavior | Pros | Cons |
|---|---|---|
| **Blur = cancel** (recommended) | Matches existing Escape semantics; no accidental commits; user intent ("I clicked away") = abandon | User who typed a name and clicked away loses input |
| Blur = submit-if-non-empty, cancel-if-empty | One less keystroke to commit | Surprise commits; conflicts with Escape=cancel; a stray click relabels a whole cluster |

**Recommendation: blur = cancel.** The cluster rename is a destructive-ish,
meeting-wide action (every segment in the cluster relabels). Accidental commits
are worse than requiring an explicit Enter. This also keeps `onBlur` and
`onKeyDown:Escape` semantically identical, so there is one mental model.

### D2 — Focus-preserving guard on suggestion chips

Add `onMouseDown={(e) => e.preventDefault()}` to each suggestion-chip `<button>`
(`SpeakerBadge.tsx:134`). `preventDefault` on `mousedown` suppresses the
default focus shift, so the input retains focus, the `click` event fires
normally, and `onSubmit(name)` runs. This is the idiomatic React pattern for
"don't blur my input when I click a sibling button" and avoids `setTimeout`
deferral hacks (which introduce flicker and ordering ambiguity).

### D3 — Double-fire analysis (synchronous paths are safe; one pre-existing async race, out of scope)

`onSubmit` (Enter) does not trigger blur, so the Enter path cannot race with
`onCancel`. The chip path is guarded by D2's `preventDefault`. Genuine
click-outside should cancel. These synchronous paths need no `useRef`
"already submitted" sentinel.

One race exists but is **pre-existing and not introduced by this change**, so
it is out of scope (file separately per the scoped-changes convention): if the
user clicks a second speaker badge while the first's `labelSpeaker` is still
in flight, the first's eventual `setEditingSegmentId(null)` (on resolve/reject)
clobbers the second edit. This race exists in the current code without
blur-cancel; this change neither introduces nor fixes it. A `committedRef`
guard is the fix if it becomes a real complaint — YAGNI until then. The blur
change does not worsen it: blur-cancel on the first input dismisses it cleanly,
and the clobber is caused by the async-resolve, not by blur.

### D4 — Hexagonal boundary

`SpeakerLabelInput` is a presentational React component in `ui/`-adjacent
territory. The change touches only DOM event handlers — no `invoke()`, no
adapter, no port, no domain type. No hexagonal boundary is crossed; this is
purely a UI-affordance fix.

### D5 — Adversarial / edge cases (§4 frontend-relevant subset)

| Case | Expected |
|---|---|
| Click outside (empty input) | `onCancel`; input unmounts; no command dispatched |
| Click outside (non-empty input) | `onCancel`; input unmounts; **no** `label_speaker` dispatched (the whole point — no accidental commit) |
| Click suggestion chip | `onSubmit(name)`; input unmounts; `label_speaker` dispatched with the chip name |
| Press Escape | `onCancel` (unchanged) |
| Press Enter with text | `onSubmit` (unchanged) |
| Click a second badge while first is open | First input's `onBlur`→`onCancel` unmounts it; second opens (driven by `editingSegmentId` state) |
| Tab away from input | `onBlur`→`onCancel` (acceptable; the input is a transient affordance, not a form field users tab through) |

The Tab case is worth calling out: tab-cancel is a minor cost of blur-cancel.
Inline-edit controls in this codebase are not part of a tab-order form flow, so
cancel-on-tab is acceptable and consistent with click-outside.

## Alternatives considered

- **Submit-on-blur (D1 rejected):** rejected per D1 — accidental cluster-wide
  relabel is the higher cost.
- **`setTimeout(0)` deferred cancel:** defer `onCancel` so a chip click lands
  first. Works but introduces a visible flicker (input briefly persists after
  blur) and ordering ambiguity under slow dispatch. D2's `preventDefault` is
  cleaner.
- **Click-outside hook (refs + document listener):** a `useClickOutside` hook
  on the container. More code than the bug warrants; `onBlur` on the input
  already captures "focus left the field" which is the actual user gesture.

## Risks

- **Tab-cancel surprise:** users who tab through the input will cancel. Mitigated
  by the input being a transient inline affordance, not a form field. Flagged in
  D5; acceptable.
- **Chip-onMouseDown on touch devices:** `preventDefault` on `mousedown` is
  fine for mouse; touch devices fire `touchstart`/`pointerdown`. Playwright
  smoke runs on the desktop engine (WebView2/WKWebView), so the guard is
  exercised on the platforms that ship. If a mobile target is ever added, this
  needs a touch-audit; not a concern today (Tauri desktop only).

## Open questions

- Should clicking another transcript row's badge while one is open preserve the
  first input's typed text? Currently no (blur cancels). This matches most
  inline-edit UIs. No change unless a user reports it.
