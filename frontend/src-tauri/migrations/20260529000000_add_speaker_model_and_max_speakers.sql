-- Speaker model selection and max speakers cap
ALTER TABLE settings ADD COLUMN speaker_embedding_model TEXT NOT NULL DEFAULT '3dspeaker';
ALTER TABLE settings ADD COLUMN max_speakers INTEGER NOT NULL DEFAULT 10;
