## Context

`DatabaseManager._init_db()` currently delegates entirely to `_legacy_init_db()`, which does three things in one function: creates all six tables (`CREATE TABLE IF NOT EXISTS`), runs four ad-hoc column migrations (`ALTER TABLE ADD COLUMN`, swallowed on `OperationalError`), and seeds the default settings row (`INSERT OR IGNORE`). The "legacy" prefix signals intent to replace, but this is the live implementation. `schema_validator.py` references the function by name in a comment; the `local-first-defaults` spec references it in scenario text.

## Goals / Non-Goals

**Goals:**
- Replace `_legacy_init_db()` with two named methods: `_create_schema()` (DDL + migrations) and `_seed_defaults()` (INSERT OR IGNORE seed).
- Update all four non-production files that reference the old name (comment, docstring, spec scenario text).
- Keep `_init_db()` as the thin orchestrator it already is.

**Non-Goals:**
- Converting sync `sqlite3` init to async `aiosqlite` — not a practical problem today.
- Removing or versioning the `ALTER TABLE` migration blocks — separate concern.
- Any behavior change whatsoever.

## Decisions

**Split at the seam between DDL and DML** — `_create_schema()` owns everything up to and including `conn.commit()` except the seed insert. `_seed_defaults()` opens its own connection for the single `INSERT OR IGNORE`. This keeps each method independently testable and matches the natural read order: schema first, data second.

**Why not a single rename?** A flat rename to `_init_schema()` would still bundle seeding with DDL. The seed is already tested in isolation (`test_db_local_first_defaults.py`) and referenced independently in the spec — it deserves its own name.

**`_init_db()` call order stays the same:**
```python
def _init_db(self):
    self._create_schema()
    self._seed_defaults()
    self.schema_validator.validate_schema()
```

## Risks / Trade-offs

- **Tests pass through `DatabaseManager()`** — none call `_legacy_init_db()` directly, so a rename won't break them. The docstring update is cosmetic. → No test risk.
- **`schema_validator` comment** — purely documentation, no runtime effect. → No runtime risk.
- **Spec scenario text** — references `_legacy_init_db()` in a THEN clause. Updating it is a spec correction, not a requirement change. → No behavioral impact.
