## ADDED Requirements

### Requirement: Per-request LLM timeout
The Rust Tauri process SHALL apply a maximum 900-second timeout to every HTTP request sent to an LLM provider (Ollama, OpenAI, Claude, Groq, OpenRouter, CustomOpenAI). If the request exceeds this limit, it SHALL return an error message stating the request "timed out after 15 minutes".

#### Scenario: Request completes within timeout
- **WHEN** an LLM provider responds within 900 seconds
- **THEN** the response is returned successfully with no timeout error

#### Scenario: Request exceeds timeout
- **WHEN** an LLM provider does not respond within 900 seconds
- **THEN** the system returns an error containing the text "timed out after 15 minutes"

#### Scenario: Request cancelled before timeout
- **WHEN** the user cancels summary generation before the 900-second limit is reached
- **THEN** the system returns a cancellation error, not a timeout error
