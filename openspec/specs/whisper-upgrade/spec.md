## Requirements

### Requirement: Rust build succeeds on Windows MSVC
The `cargo check` command SHALL complete without errors on Windows MSVC after the whisper-rs upgrade. No anonymous-struct bindgen errors (`no field 'greedy' on type 'whisper_full_params'`) SHALL appear.

#### Scenario: Clean cargo check on Windows
- **WHEN** `cargo check` is run in `frontend/` on a Windows MSVC toolchain
- **THEN** the command exits with code 0 and zero errors related to `whisper_full_params` field access

### Requirement: Transcription behaviour is preserved
After the upgrade, the transcription pipeline SHALL produce equivalent output for the same audio input as before the upgrade.

#### Scenario: Existing model loads without error
- **WHEN** the app starts and loads the `large-v3-turbo` whisper model
- **THEN** the model loads successfully with no runtime errors

#### Scenario: Transcription produces non-empty text for speech audio
- **WHEN** a short audio clip containing speech is transcribed
- **THEN** the returned transcript text is non-empty and contains recognisable words
