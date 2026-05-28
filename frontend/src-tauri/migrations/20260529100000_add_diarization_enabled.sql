-- Toggle for speaker diarization
ALTER TABLE settings ADD COLUMN diarization_enabled INTEGER NOT NULL DEFAULT 1;
