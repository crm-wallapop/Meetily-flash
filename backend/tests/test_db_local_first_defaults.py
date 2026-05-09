"""
Tests for fix-local-first-defaults:
- _create_schema() + _seed_defaults() seeds id='1' with Ollama defaults on fresh install
- _seed_defaults() never overwrites an existing id='1' row
- save_api_key() fallback INSERT uses Ollama defaults
"""
import asyncio
import sqlite3
import tempfile
import os
import sys
import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))
from app.db import DatabaseManager


def make_db(tmp_path: str) -> DatabaseManager:
    return DatabaseManager(db_path=tmp_path)


# ---------------------------------------------------------------------------
# Task 1.1 — fresh install: create_tables() seeds id='1' with Ollama defaults
# ---------------------------------------------------------------------------

def test_create_tables_seeds_ollama_defaults_on_fresh_install(tmp_path):
    db_file = str(tmp_path / "fresh.db")
    make_db(db_file)

    with sqlite3.connect(db_file) as conn:
        row = conn.execute(
            "SELECT provider, model, whisperModel FROM settings WHERE id = '1'"
        ).fetchone()

    assert row is not None, "Settings row id='1' must exist after create_tables()"
    provider, model, whisper_model = row
    assert provider == "ollama", f"Expected provider 'ollama', got '{provider}'"
    assert model == "qwen3.5:4b", f"Expected model 'qwen3.5:4b', got '{model}'"
    assert whisper_model == "large-v3-turbo", f"Expected whisper_model 'large-v3-turbo', got '{whisper_model}'"


# ---------------------------------------------------------------------------
# Task 1.2 — existing install: create_tables() must NOT overwrite existing row
# ---------------------------------------------------------------------------

def test_create_tables_does_not_overwrite_existing_settings(tmp_path):
    db_file = str(tmp_path / "existing.db")

    # Pre-populate with a user-configured row
    with sqlite3.connect(db_file) as conn:
        conn.execute("""
            CREATE TABLE IF NOT EXISTS settings (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                whisperModel TEXT NOT NULL,
                groqApiKey TEXT,
                openaiApiKey TEXT,
                anthropicApiKey TEXT,
                ollamaApiKey TEXT
            )
        """)
        conn.execute(
            "INSERT INTO settings (id, provider, model, whisperModel) VALUES ('1', 'claude', 'claude-opus-4', 'tiny')"
        )
        conn.commit()

    # Now init DatabaseManager — should not overwrite
    make_db(db_file)

    with sqlite3.connect(db_file) as conn:
        row = conn.execute(
            "SELECT provider, model, whisperModel FROM settings WHERE id = '1'"
        ).fetchone()

    assert row is not None
    provider, model, whisper_model = row
    assert provider == "claude", f"Existing provider must be preserved, got '{provider}'"
    assert model == "claude-opus-4", f"Existing model must be preserved, got '{model}'"
    assert whisper_model == "tiny", f"Existing whisper_model must be preserved, got '{whisper_model}'"


# ---------------------------------------------------------------------------
# Task 1.3 — save_api_key() fallback INSERT uses Ollama defaults
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_save_api_key_fallback_uses_ollama_defaults(tmp_path):
    db_file = str(tmp_path / "fallback.db")
    db = make_db(db_file)

    # Delete the seeded row so save_api_key hits the INSERT branch
    with sqlite3.connect(db_file) as conn:
        conn.execute("DELETE FROM settings WHERE id = '1'")
        conn.commit()

    # Trigger save_api_key with an Ollama key — the fallback INSERT should use Ollama defaults
    await db.save_api_key("test-key-123", "ollama")

    with sqlite3.connect(db_file) as conn:
        row = conn.execute(
            "SELECT provider, model, whisperModel FROM settings WHERE id = '1'"
        ).fetchone()

    assert row is not None, "save_api_key fallback must insert a settings row"
    provider, model, whisper_model = row
    assert provider == "ollama", f"Fallback provider must be 'ollama', got '{provider}'"
    assert model == "qwen3.5:4b", f"Fallback model must be 'qwen3.5:4b', got '{model}'"
    assert whisper_model == "large-v3-turbo", f"Fallback whisper_model must be 'large-v3-turbo', got '{whisper_model}'"
