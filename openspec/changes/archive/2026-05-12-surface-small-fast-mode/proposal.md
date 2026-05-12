## Why

Transcription of a 28-minute meeting currently takes ~18 minutes at default quality (`large-v3-turbo-q5_0`). A measured performance sweep showed that `small-q5_1` completes the same meeting in 6:23 — 3.45× faster — with only ~4% word loss and no meaningful accuracy degradation for meeting content. Users who need fast turnaround have no way to select this model because it is buried in the "Advanced Models" accordion, invisible by default.

## What Changes

- `small-q5_1` is promoted from the hidden "Advanced Models" accordion to the visible "Basic Models" list in the model selection UI.
- The display name is updated to "Small (Fast Mode)" with a subtitle that communicates the speed/accuracy trade-off.
- The catalog description in `config.rs` is updated to reference the measured gain (3.45× faster, ~4% accuracy trade-off).
- No default model change — `large-v3-turbo` remains the default.

## Capabilities

### New Capabilities

- `whisper-model-selection`: The model selection UI surface — which models are visible, how they are labelled, and what context users see when choosing a model.

### Modified Capabilities

_(none — no existing spec files exist to delta against)_

## Impact

- `frontend/src/components/WhisperModelManager.tsx` — `basicModelNames` array and `getDisplayName()` function.
- `frontend/src-tauri/src/config.rs` — description string for `small-q5_1` in `WHISPER_MODEL_CATALOG`.
- No API changes, no database changes, no Tauri command changes.
