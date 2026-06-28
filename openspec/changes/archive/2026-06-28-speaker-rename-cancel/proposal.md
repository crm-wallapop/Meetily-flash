# Proposal: speaker-rename-cancel

## Why

When the user clicks a speaker badge to rename it, the inline `SpeakerLabelInput`
(`SpeakerBadge.tsx:96`) grabs focus (`autoFocus`, line 120) and handles **Enter**
(submit) and **Escape** (cancel) via `onKeyDown` (lines 103–109). It has **no
`onBlur` handler**. The only way to dismiss the input without committing is to
press Escape — there is no affordance for clicking elsewhere. Users who click
outside the input (the intuitive "I changed my mind" gesture) see the text box
stay focused and trapped open; from their report it feels like the override
action "can't be cancelled."

The Escape path works, so this is a discoverability + affordance bug, not a
logic bug. The fix is small (one `onBlur`, one focus-preserving guard on the
suggestion chips) but the UX payoff is disproportionate: click-outside cancel is
the default expectation for any inline-edit control.

## What Changes

- Add `onBlur={onCancel}` to the `<input>` in `SpeakerLabelInput` so that losing
  focus dismisses the input without committing — matching the existing Escape
  semantics.
- Add `onMouseDown={(e) => e.preventDefault()}` on the suggestion-chip buttons so
  that selecting a suggested name does not blur the input before the chip's
  `onClick`/`onSubmit` can fire (otherwise blur→`onCancel` unmounts the input
  and the chip click is lost — the canonical React inline-edit race).
- No backend, port, adapter, command, or repository changes. No new Tauri
  commands. No DB schema changes.

## Capabilities

- `speaker-diarization` — narrows the inline-labeling UX with a cancel-on-blur
  guarantee on the badge input.

## Impact

- **Files touched:** `frontend/src/components/SpeakerBadge.tsx` only (the shared
  component consumed by both `TranscriptView.tsx:325` and
  `VirtualizedTranscriptView.tsx:127`).
- **Behavior change:** clicking outside the open speaker-name input now cancels
  (previously: no-op, input stayed open). Pressing a suggestion chip still
  submits. Pressing Escape still cancels. Pressing Enter still submits.
- **Risk:** low. Pure additive DOM-event wiring on a presentational component.
  No data flow, state, or persistence changes.
- **Sequencing:** independent of `diarization-temporal-coherence` (backend) and
  `per-turn-speaker-override` (same files). If both UI changes land, apply this
  one first — it is the smaller delta and the per-turn change will re-touch
  `SpeakerBadge.tsx`.
