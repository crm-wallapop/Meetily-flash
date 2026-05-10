## Context

`whisper-rs 0.13.2` fails on Windows MSVC because `whisper-rs-sys` bindgen generates the anonymous C structs inside `whisper_full_params` (`greedy`, `beam_search`) as a single `_address` field. The `whisper-rs` high-level crate then tries to access `fp.greedy.best_of` and `fp.beam_search.beam_size`, producing ~40 `no field` compile errors.

The codebase (`whisper_engine/whisper_engine.rs`) already uses the `SamplingStrategy` enum API introduced in an earlier minor version — `FullParams::new(SamplingStrategy::BeamSearch { ... })` is present at lines 526 and 643. No raw field access to the broken structs exists in our code. The breakage is entirely inside the `whisper-rs` crate itself.

`whisper-rs 0.16.0` is the latest release. It is the first version where the anonymous-struct MSVC issue is resolved in `whisper-rs-sys`.

## Goals / Non-Goals

**Goals:**
- `cargo check` passes on Windows MSVC with no `_address` / anonymous struct errors
- Transcription behaviour is identical before and after the upgrade
- All three platform targets (macOS, Windows, Linux) updated to 0.16.0

**Non-Goals:**
- Adopting any new 0.16.0 features (VAD via whisper, grammar decoding, safe callbacks)
- Changing transcription parameters, model loading, or audio pipeline behaviour
- Upgrading `whisper-rs-sys` independently (it is a transitive dependency, bumped automatically)

## Decisions

### Version bump only until proven otherwise

The existing call sites already use `SamplingStrategy`, `WhisperContextParameters`, `FullParams::new()`, and state methods (`full`, `full_n_segments`, `full_get_segment_text_lossy`, `full_get_segment_t0/t1`). None of these access the broken internal structs directly.

**Decision**: Start with a straight version bump (`0.13.2` → `0.16.0`) in all three platform-target sections of `Cargo.toml`. Let `cargo check` report any actual API breakage before writing migration code. This avoids speculative fixes.

**Alternative considered**: Patch `whisper-rs 0.13.2` with a `[patch.crates-io]` pointing to a fork. Rejected: maintaining a fork adds permanent overhead; the upstream fix in 0.16.0 is the canonical resolution.

### No feature flag changes

The existing features (`raw-api`, `metal`, `coreml`, `vulkan`) must be verified as still valid in 0.16.0. If a feature was renamed, update accordingly. The `raw-api` feature enables access to `WhisperContextParameters` — it must remain enabled.

## Risks / Trade-offs

- **Transitive dependency churn** → `Cargo.lock` will change (new `whisper-rs-sys` version). Mitigation: review `cargo check` output for unexpected new errors beyond whisper-rs itself.
- **API breakage between minor versions** → The high-level API has been stable, but minor versions can have undocumented breaking changes in Rust crates. Mitigation: `cargo check` will surface every call site that breaks.
- **whisper.cpp model compatibility** → `whisper-rs-sys` bundles whisper.cpp source. A version bump may embed a newer whisper.cpp version with different model weight expectations. Mitigation: test transcription with the existing `large-v3-turbo` model after the bump.

## Migration Plan

1. Bump all three `whisper-rs` entries in `Cargo.toml` from `0.13.2` to `0.16.0`
2. Run `cargo check` — fix any compile errors surfaced
3. Run `cargo build` to confirm full build (not just check)
4. Smoke-test: start the app, load a model, transcribe a short audio clip
5. Rollback: revert `Cargo.toml` and delete `Cargo.lock` to restore previous state
