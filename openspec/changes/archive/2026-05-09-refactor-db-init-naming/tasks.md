## 1. Refactor db.py

- [x] 1.1 Rename `_legacy_init_db()` to `_create_schema()` — same body, new name
- [x] 1.2 Extract the `INSERT OR IGNORE` seed block from `_create_schema()` into a new `_seed_defaults()` method with its own `sqlite3.connect` context
- [x] 1.3 Update `_init_db()` to call `_create_schema()` then `_seed_defaults()` (before `schema_validator.validate_schema()`)

## 2. Update referencing files

- [x] 2.1 Update comment in `backend/app/schema_validator.py` (line ~33) from `_legacy_init_db` to `_create_schema`
- [x] 2.2 Update docstring in `backend/tests/test_db_local_first_defaults.py` to reference `_create_schema()` and `_seed_defaults()`

## 3. Verification

- [x] 3.1 Run `backend/venv/Scripts/python -m pytest backend/tests/ -v` — all 3 tests green
