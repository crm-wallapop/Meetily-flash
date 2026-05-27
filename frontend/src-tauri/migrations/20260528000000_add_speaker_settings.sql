-- Speaker diarization settings
ALTER TABLE settings ADD COLUMN speaker_merge_threshold REAL NOT NULL DEFAULT 0.50;
