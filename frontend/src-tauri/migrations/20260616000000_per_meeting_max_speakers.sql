-- Per-meeting max_speakers override. NULL = inherit global settings.max_speakers.
ALTER TABLE meetings ADD COLUMN max_speakers INTEGER;
