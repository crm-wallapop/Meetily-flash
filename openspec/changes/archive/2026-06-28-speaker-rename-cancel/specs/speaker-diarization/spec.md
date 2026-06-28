## ADDED Requirements

### Requirement: Inline speaker-label input cancels on blur and preserves suggestion-chip submission

The inline `SpeakerLabelInput` SHALL cancel (dismiss without committing) when its text field loses focus, producing the same effect as pressing Escape; this requirement amends the "Retroactive speaker labeling via inline badges with per-speaker revert" requirement, which governs the open/submit/revert flow but is silent on dismiss mechanics. Cancelling on blur SHALL NOT dispatch `label_speaker`. Suggestion-chip buttons inside the input SHALL suppress the default focus shift on activation (via `preventDefault` on `mousedown`) so that selecting a suggested name submits the name via `onSubmit` rather than triggering blur-cancel and unmounting the input before the chip's click is delivered. Pressing Enter with non-empty text SHALL continue to submit, and pressing Escape SHALL continue to cancel. Pressing Tab (or any focus loss, including clicking a second speaker badge while one input is open) SHALL cancel, consistent with the click-outside semantics.

#### Scenario: Click outside cancels without committing

- **GIVEN** a transcript segment whose speaker badge has been clicked and the `SpeakerLabelInput` is open and focused
- **WHEN** the user clicks elsewhere in the document
- **THEN** the input is dismissed (unmounted)
- **AND** no `label_speaker` command is dispatched

#### Scenario: Typed name is discarded on click-outside

- **GIVEN** the `SpeakerLabelInput` is open with the text "Alice" typed into it
- **WHEN** the user clicks outside the input
- **THEN** the input is dismissed
- **AND** `label_speaker` is NOT dispatched (the typed name is discarded, not accidentally committed)

#### Scenario: Suggestion chip still submits after the blur guard

- **GIVEN** the `SpeakerLabelInput` is open, `knownSpeakers` is non-empty, and at least one suggestion chip matching the current typed text is visible
- **WHEN** the user clicks a visible suggestion chip
- **THEN** `label_speaker` IS dispatched with the clicked chip's name as `speakerName`
- **AND** the input is dismissed after the submit

#### Scenario: Keyboard paths are unchanged

- **GIVEN** the `SpeakerLabelInput` is open with non-empty text
- **WHEN** the user presses Enter
- **THEN** `label_speaker` is dispatched (submit) — unchanged from before this change
- **AND WHEN** the user presses Escape instead
- **THEN** the input is dismissed without dispatching `label_speaker` (cancel) — unchanged from before this change

#### Scenario: Tab and second-badge focus loss cancel (documented trade-off)

- **GIVEN** the `SpeakerLabelInput` is open with text typed into it
- **WHEN** the user presses Tab, or clicks a second speaker badge while the first input is open
- **THEN** the first input is dismissed (cancel) without dispatching `label_speaker`
- **AND** this is an intentional, documented trade-off: the input is a transient inline affordance, not a tab-stop in a form flow
