## Why

`whisper-rs 0.13.2` fails to compile on Windows MSVC because bindgen renders anonymous structs inside `whisper_full_params` (`greedy`, `beam_search`, and all other named fields) as a single `_address` field — blocking every Rust build on Windows. Upgrading to 0.16.0 resolves this and unlocks all deferred Tauri/audio items.

## What Changes

- Bump `whisper-rs` from `0.13.2` to `0.16.0` in `frontend/src-tauri/Cargo.toml` (all three platform targets)
- Update call sites that construct `FullParams` to pass a `SamplingStrategy` enum (breaking API change in whisper-rs)
- Update any callback registrations that changed between versions
- Verify `cargo check` passes on Windows MSVC (no anonymous struct errors)

## Capabilities

### New Capabilities

*(none — this is a dependency upgrade, not a user-facing capability)*

### Modified Capabilities

*(none — transcription behavior is unchanged; only the Rust build is fixed)*

## Impact

- `frontend/src-tauri/Cargo.toml` — version bump on three `[target.*.dependencies]` sections
- `frontend/src-tauri/src/whisper_engine/whisper_engine.rs` — primary call site for `FullParams::new()` and param configuration; requires `SamplingStrategy` argument
- Any other files in `frontend/src-tauri/src/` that call whisper-rs APIs directly
- `Cargo.lock` will be regenerated with new transitive dependencies
