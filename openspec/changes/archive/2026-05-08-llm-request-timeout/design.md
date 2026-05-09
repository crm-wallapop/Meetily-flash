## Context

The Rust Tauri process handles all LLM HTTP requests via `summary/llm_client.rs`. A single constant (`REQUEST_TIMEOUT_DURATION`) controls the `reqwest` client `.timeout()` applied to every provider call (Ollama, OpenAI, Claude, Groq, OpenRouter, CustomOpenAI). The timeout was set at 300s but the error messages branching on `e.is_timeout()` have always hardcoded the string "60 seconds", creating a confusing mismatch for users.

The 300s ceiling is too tight for CPU-only Ollama inference on low-end hardware. A 4,371-word transcript (~1,530 tokens by the rough estimator in `processor.rs`) fits in a single chunk and took 3–5 min on Intel Arc integrated, leaving almost no margin before the timeout fires. Raising to 900s gives the same user a comfortable buffer while still surfacing a hung-Ollama condition within 15 minutes.

## Goals / Non-Goals

**Goals:**
- Raise the per-request timeout to 900s in the single constant that controls all providers.
- Fix the two error message strings to match the actual timeout value ("15 minutes").

**Non-Goals:**
- Setting different timeouts per provider (not needed yet; 900s is safe for all).
- Adding a timeout to the Python backend's `chat_ollama_model()` (tracked as Item 7, separate change).
- Making the timeout user-configurable (YAGNI — one sensible constant is enough).
- Changing the frontend polling ceiling (MAX_POLLS = 200 × 5s ≈ 16.5 min already exceeds 15 min).

## Decisions

**D1: 900s (15 min), not 1800s (30 min)**

900s is generous enough for CPU inference with a 4b model on old hardware. 1800s would make a hung-Ollama situation take 30 min to surface — too long for a background task the user is still waiting on. 15 min matches the comment already in `SidebarProvider.tsx` (`"slightly longer than backend's 15-min timeout"`), suggesting this value was always the intended ceiling.

**D2: Single constant, not per-provider values**

All providers share `REQUEST_TIMEOUT_DURATION`. Splitting by provider adds complexity for no current benefit — cloud providers respond in seconds, so the 900s ceiling doesn't hurt them.

**D3: Error message says "15 minutes", not a computed value**

The message is a static string. Computing it from the constant at runtime (e.g., `format!("timed out after {} seconds", REQUEST_TIMEOUT_DURATION.as_secs())`) would be more robust but is unnecessary complexity for a value that won't change without an intentional code edit.

## Risks / Trade-offs

- **Multi-chunk timeout multiplication**: A transcript split into N chunks has a theoretical max of (N+1) × 900s. For very long transcripts on slow hardware this could exceed the frontend's 16.5-min polling window. Accepted: chunks are much shorter than the full transcript, so per-chunk inference is fast even on CPU.
- **Python backend gap remains**: `chat_ollama_model()` in the Python process still has no timeout. If the Python path is triggered (rather than the Rust path), a hung Ollama call will hang the backend process indefinitely. Mitigated by Item 7 in the backlog.
