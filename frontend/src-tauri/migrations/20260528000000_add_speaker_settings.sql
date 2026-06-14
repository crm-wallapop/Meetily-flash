-- Speaker diarization settings
ALTER TABLE settings ADD COLUMN speakerMergeThreshold REAL NOT NULL DEFAULT 0.50;
