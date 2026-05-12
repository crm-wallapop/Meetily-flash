## 1. Rust catalog update

- [x] 1.1 In `frontend/src-tauri/src/config.rs`, update the `small-q5_1` entry in `WHISPER_MODEL_CATALOG` — change description from `"Quantized small model, faster than f16 version"` to `"Fast mode: 3.5× faster than default, ~4% accuracy trade-off"`

## 2. UI promotion

- [x] 2.1 In `frontend/src/components/WhisperModelManager.tsx`, add `"small-q5_1"` to the `basicModelNames` array so it appears in the primary model list
- [x] 2.2 In `frontend/src/components/WhisperModelManager.tsx`, add a `"small-q5_1"` → `"Small (Fast Mode)"` mapping in `getDisplayName()`

## 3. Verification

- [x] 3.1 Run `cargo check --features vulkan` in `frontend/src-tauri/` to confirm `config.rs` compiles clean
- [x] 3.2 Run `pnpm lint` in `frontend/` to confirm TypeScript changes pass linting
