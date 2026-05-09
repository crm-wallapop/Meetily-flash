## 1. Tests

- [x] 1.1 Write a failing test that verifies `_legacy_init_db()` seeds `id='1'` with ollama/qwen3.5:4b/large-v3-turbo when the settings table is empty
- [x] 1.2 Write a failing test that verifies `_legacy_init_db()` does NOT overwrite an existing `id='1'` row
- [x] 1.3 Write a failing test that verifies the `save_api_key()` fallback INSERT uses ollama/qwen3.5:4b/large-v3-turbo

## 2. Implementation

- [x] 2.1 In `_legacy_init_db()`, add `INSERT OR IGNORE INTO settings (id, provider, model, whisper_model) VALUES ('1', 'ollama', 'qwen3.5:4b', 'large-v3-turbo')` immediately after the `CREATE TABLE IF NOT EXISTS settings` statement
- [x] 2.2 In `save_api_key()`, change the fallback INSERT defaults from `'openai'`/`'gpt-4o-2024-11-20'`/`'large-v3'` to `'ollama'`/`'qwen3.5:4b'`/`'large-v3-turbo'`

## 3. Verification

- [x] 3.1 Run `pytest backend/` — all tests green
