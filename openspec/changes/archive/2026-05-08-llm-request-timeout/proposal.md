## Why

The Rust LLM client (`llm_client.rs`) has a 300-second per-request timeout that is too short for CPU inference with Ollama on low-end hardware (a 4,371-word transcript took 3–5 minutes on Intel Arc integrated graphics), and its timeout error messages have always incorrectly said "60 seconds" regardless of the actual configured value.

## What Changes

- Raise `REQUEST_TIMEOUT_DURATION` in `llm_client.rs` from 300s to 900s (15 minutes) to accommodate slow CPU inference with local Ollama models.
- Fix two error message strings that hardcode "60 seconds" to instead say "15 minutes", matching the actual timeout value.

## Capabilities

### New Capabilities

- `llm-request-timeout`: Governs the per-HTTP-request timeout applied to all LLM provider calls (Ollama, OpenAI, Claude, Groq, OpenRouter, CustomOpenAI) in the Rust Tauri process.

### Modified Capabilities

<!-- No existing specs are changing requirements — this is a new capability spec. -->

## Impact

- **`frontend/src-tauri/src/summary/llm_client.rs`**: lines 8, 272, 285.
- No API surface changes. No new dependencies. No database changes.
- Users on slow hardware will now wait up to 15 minutes per LLM chunk before seeing a timeout error instead of 5 minutes. Multi-chunk transcripts multiply this ceiling but each individual chunk is much shorter than the full transcript.
