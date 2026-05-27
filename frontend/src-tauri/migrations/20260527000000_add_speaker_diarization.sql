-- Migration: Add speaker diarization support
-- Adds speaker label, token timestamps, and speaker_source to transcripts.
-- Creates speakers and speaker_embeddings tables for cross-meeting identification.

-- Speaker label from diarization (distinct from existing 'speaker' column which stores mic/system)
ALTER TABLE transcripts ADD COLUMN speaker_label TEXT;
ALTER TABLE transcripts ADD COLUMN token_timestamps TEXT;
ALTER TABLE transcripts ADD COLUMN speaker_source TEXT;

-- Named speakers with persistent colors
CREATE TABLE IF NOT EXISTS speakers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    color TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Speaker embeddings for cross-meeting matching
CREATE TABLE IF NOT EXISTS speaker_embeddings (
    id TEXT PRIMARY KEY,
    speaker_id TEXT,
    embedding BLOB NOT NULL,
    source_meeting_id TEXT NOT NULL,
    cluster_label TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (speaker_id) REFERENCES speakers(id) ON DELETE SET NULL,
    FOREIGN KEY (source_meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);
