## Why

`_legacy_init_db()` conflates three distinct concerns (schema creation, column migrations, and default seeding) under a name that implies it will be replaced — but it is the real implementation. The name misleads contributors and makes the `_seed_defaults` logic hard to find or test in isolation. Now that we have a spec and tests referencing this function by name, cleaning up the name pays forward.

## What Changes

- `backend/app/db.py` — rename `_legacy_init_db()` to `_create_schema()` and extract the `INSERT OR IGNORE` seed block into a new `_seed_defaults()` method. `_init_db()` calls both in sequence.
- `backend/app/schema_validator.py` — update the comment at line 33 that references `_legacy_init_db` by name.
- `backend/tests/test_db_local_first_defaults.py` — update docstring to reference `_create_schema()` and `_seed_defaults()`.
- `openspec/specs/local-first-defaults/spec.md` — update scenario text that names `_legacy_init_db()`.

No behavior changes. No API changes. The `ALTER TABLE` migration blocks stay inside `_create_schema()` unchanged — migration tracking is a separate concern.

## Capabilities

### New Capabilities

<!-- None -->

### Modified Capabilities

- `local-first-defaults`: Scenario text references `_legacy_init_db()` by name — update to `_create_schema()` / `_seed_defaults()` to reflect the new structure. Requirements themselves are unchanged.

## Impact

- `backend/app/db.py` (rename + extract)
- `backend/app/schema_validator.py` (comment only)
- `backend/tests/test_db_local_first_defaults.py` (docstring only)
- `openspec/specs/local-first-defaults/spec.md` (scenario text only)
