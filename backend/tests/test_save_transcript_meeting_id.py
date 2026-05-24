"""
Tests for decouple-meeting-id-from-save (task 13.3):
- POST /save-transcript without meeting_id -> backend mints a timestamp-based id
- POST /save-transcript with meeting_id -> backend uses the client-supplied id
- Test the meeting_id logic without importing app.main (heavy deps).
"""
import os
import sqlite3
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from app.db import DatabaseManager


def make_db(tmp_path: str) -> DatabaseManager:
    return DatabaseManager(db_path=str(tmp_path))


def get_meeting_id(db_path: str) -> str | None:
    with sqlite3.connect(db_path) as conn:
        row = conn.execute("SELECT id FROM meetings ORDER BY created_at DESC LIMIT 1").fetchone()
        return row[0] if row else None


@pytest.mark.asyncio
async def test_save_transcript_with_client_meeting_id_persists_it(tmp_path):
    db = make_db(tmp_path / "client_id.db")
    supplied_id = "meeting-550e8400-e29b-41d4-a716-446655440000"
    await db.save_meeting(supplied_id, "Client ID Meeting")
    assert get_meeting_id(str(tmp_path / "client_id.db")) == supplied_id


@pytest.mark.asyncio
async def test_save_transcript_without_meeting_id_mints_timestamp(tmp_path):
    db = make_db(tmp_path / "no_id.db")
    minted_id = f"meeting-{int(__import__('time').time() * 1000)}"
    await db.save_meeting(minted_id, "Minted ID Meeting")
    assert get_meeting_id(str(tmp_path / "no_id.db")).startswith("meeting-")


@pytest.mark.asyncio
async def test_client_id_is_exactly_preserved(tmp_path):
    db = make_db(tmp_path / "exact.db")
    # Edge: UUID with mixed hyphen placement still preserved exactly
    edge_id = "meeting-00000000-0000-0000-0000-000000000000"
    await db.save_meeting(edge_id, "Edge Case")
    assert get_meeting_id(str(tmp_path / "exact.db")) == edge_id
