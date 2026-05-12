## Context

The `WhisperModelManager` component controls which models are shown prominently to users. It splits the catalog into two tiers:
- **Basic models** — shown directly in the main list (currently: `small`, `medium-q5_0`, `large-v3-q5_0`, `large-v3-turbo`, `large-v3`).
- **Advanced models** — hidden in a collapsed accordion, requiring an extra click to see.

`small-q5_1` lives in the advanced tier despite being 3.45× faster than the default with only ~4% word loss — a trade-off most users would willingly accept for everyday use. The catalog entry in `config.rs` also has a generic description with no performance context.

## Goals / Non-Goals

**Goals:**
- Promote `small-q5_1` to the basic/visible tier so users can discover and select the fast mode.
- Give it a display name and description that communicate the performance vs. accuracy trade-off concisely.
- Update the catalog description in `config.rs` to reflect measured data.

**Non-Goals:**
- Changing the default model (stays `large-v3-turbo`).
- Adding new UI controls, tooltips, or modal explanations.
- Modifying any Rust transcription logic.
- Altering model download or loading behaviour.

## Decisions

### D1: Promote to `basicModelNames`, not a new UI element

**Decision:** Add `"small-q5_1"` to the existing `basicModelNames` array in `WhisperModelManager.tsx`.

**Rationale:** The tier split is already the right mechanism. Introducing a dedicated "Fast Mode" toggle would be a larger UX change and out of scope. Promoting within the existing structure is minimal and reversible.

**Alternative considered:** A separate "Fast Mode" badge or pill. Rejected — adds UX complexity without new information.

### D2: Display name "Small (Fast Mode)"

**Decision:** Map `"small-q5_1"` → `"Small (Fast Mode)"` in `getDisplayName()`.

**Rationale:** The parenthetical signals intent without requiring users to decode quantisation notation. Consistent with the existing pattern where `getDisplayName()` maps catalog names to human labels.

**Alternative considered:** "Fast Mode (small-q5_1)" — rejected, model family first is more scannable.

### D3: Update catalog description with measured numbers

**Decision:** Change the `config.rs` description from `"Quantized small model, faster than f16 version"` to `"Fast mode: 3.5× faster than default, ~4% accuracy trade-off"`.

**Rationale:** Users picking a model in the UI see this description. Concrete measured numbers are more useful than a relative comparison to a model variant they haven't heard of.

## Risks / Trade-offs

- **Accuracy regression perception** → Users who see "Fast Mode" and try it on a noisy or non-English-heavy meeting may be disappointed. Mitigation: description text makes the trade-off explicit before selection.
- **Model not downloaded** → If the user selects `small-q5_1` without having downloaded it, the existing download flow handles it. No new error path introduced.

## Migration Plan

No migration needed. The change is purely additive — existing user model selection is preserved, the default is unchanged, and the catalog entry is already in place.
