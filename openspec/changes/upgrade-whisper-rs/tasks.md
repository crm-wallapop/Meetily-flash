## 1. Version Bump

- [ ] 1.1 In `frontend/src-tauri/Cargo.toml`, change all three `whisper-rs` entries (macOS, Windows, Linux platform targets) from `version = "0.13.2"` to `version = "0.16.0"`

## 2. Compile Verification

- [ ] 2.1 Run `cargo check` in `frontend/` — record any new errors beyond the expected whisper-rs anonymous-struct fix
- [ ] 2.2 Fix any API breakage surfaced by `cargo check` (update call sites in `whisper_engine/whisper_engine.rs` or other files as needed)
- [ ] 2.3 Confirm `cargo check` exits with code 0 and zero errors

## 3. Feature Flag Verification

- [ ] 3.1 Verify `raw-api`, `metal`, `coreml`, and `vulkan` feature flags still exist in whisper-rs 0.16.0 (check docs.rs or `cargo check` output); rename any that changed
