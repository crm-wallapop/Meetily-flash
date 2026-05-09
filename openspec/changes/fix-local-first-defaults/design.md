## Context

`backend/app/db.py` creates the `settings` table in `_legacy_init_db()` but never seeds it. On first launch the table is empty, so the app falls back to hard-coded defaults in `save_api_key()` which point to `openai`/`gpt-4o-2024-11-20`/`large-v3`. Users must manually reconfigure before any transcription or summarisation works.

## Goals / Non-Goals

**Goals:**
- Seed the settings table with Ollama/qwen3.5:4b/large-v3-turbo on first boot.
- Fix the fallback constants in `save_api_key()` to match.
- Never overwrite an existing user configuration.

**Non-Goals:**
- UI changes or onboarding flow changes.
- Adding a settings migration system.
- Supporting multiple default profiles.

## Decisions

**`INSERT OR IGNORE` for seeding** — A single `INSERT OR IGNORE INTO settings (id, provider, model, whisper_model) VALUES ('1', 'ollama', 'qwen3.5:4b', 'large-v3-turbo')` appended inside `_legacy_init_db()` (after the `CREATE TABLE IF NOT EXISTS` statement) guarantees idempotency. If a row with `id='1'` already exists the insert is silently skipped; if the table is brand new the row is created. No migration table or version tracking is needed.

**Why `id='1'`** — The existing code reads and writes the single settings row by `id='1'`. This convention is already established in the codebase.

**Fallback constants in `save_api_key()`** — This function contains an `INSERT OR REPLACE` that acts as a last-resort default when the caller supplies no provider/model. Changing the three string literals there (provider, model, whisper_model) is the minimal fix. The function signature and callers are unchanged.

## Risks / Trade-offs

- **Existing broken installs**: Users who already have `id='1'` seeded with OpenAI values will not be updated (by design — `INSERT OR IGNORE`). They must change settings manually or delete the DB. → Acceptable: the fix targets fresh installs only.
- **`save_api_key()` fallback path**: The fallback is only reached in error/edge-case paths; changing its defaults has no effect on users with a properly seeded settings row. → Low risk.

## Migration Plan

No migration needed. The `INSERT OR IGNORE` runs every time `_legacy_init_db()` is called (at startup); the IGNORE clause makes it safe on existing databases.
