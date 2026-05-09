## Why

Fresh installs default to OpenAI with `gpt-4o-2024-11-20` and Whisper `large-v3`, requiring the user to reconfigure before the app works. Meetily-flash is a local-first fork — the factory defaults must match: Ollama as the provider, `qwen3.5:4b` as the model, and `large-v3-turbo` as the Whisper model.

## What Changes

- `backend/app/db.py` — `_legacy_init_db()` seeds a single default settings row on first boot (INSERT OR IGNORE), so the app is ready to use without manual configuration.
- `backend/app/db.py` — `save_api_key()` fallback defaults changed from `('openai', 'gpt-4o-2024-11-20', 'large-v3')` to `('ollama', 'qwen3.5:4b', 'large-v3-turbo')`.

No API, schema, or UI changes. Existing user settings are never overwritten.

## Capabilities

### New Capabilities

- `local-first-defaults`: Ensures the backend settings table is seeded with Ollama/qwen3.5:4b/large-v3-turbo on first boot, making the app functional without any user configuration.

### Modified Capabilities

<!-- None — no existing spec-level requirements are changing. -->

## Impact

- `backend/app/db.py` (two edits)
- No frontend changes
- No API contract changes
- No dependency changes
