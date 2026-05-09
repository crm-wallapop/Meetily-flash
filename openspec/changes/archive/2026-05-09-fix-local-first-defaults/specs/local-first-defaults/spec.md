## ADDED Requirements

### Requirement: Settings table is seeded on first boot
The backend SHALL insert a default settings row (`id='1'`) with provider `ollama`, model `qwen3.5:4b`, and whisper model `large-v3-turbo` when the settings table is created for the first time. The insert SHALL use `INSERT OR IGNORE` so that any existing row is never overwritten.

#### Scenario: Fresh install — table is empty
- **WHEN** the backend starts for the first time and the settings table contains no rows
- **THEN** a settings row with id=`'1'`, provider=`'ollama'`, model=`'qwen3.5:4b'`, whisper_model=`'large-v3-turbo'` SHALL exist after `_legacy_init_db()` completes

#### Scenario: Existing install — settings already configured
- **WHEN** the backend starts and the settings table already contains a row with id=`'1'`
- **THEN** the existing row SHALL remain unchanged after `_legacy_init_db()` completes

### Requirement: save_api_key fallback defaults are local-first
When `save_api_key()` falls back to inserting a default settings row (error/edge-case path), the default SHALL be provider `ollama`, model `qwen3.5:4b`, whisper model `large-v3-turbo`.

#### Scenario: Fallback insert uses local-first values
- **WHEN** `save_api_key()` executes its fallback INSERT path
- **THEN** the inserted row SHALL have provider=`'ollama'`, model=`'qwen3.5:4b'`, whisper_model=`'large-v3-turbo'`
