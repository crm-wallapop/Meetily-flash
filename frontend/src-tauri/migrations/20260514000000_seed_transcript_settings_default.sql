-- Seed default transcript_settings row so queue jobs use Parakeet by default
-- on fresh installs, matching the UI default in TranscriptSettings.
INSERT OR IGNORE INTO transcript_settings (id, provider, model)
VALUES ('1', 'parakeet', 'parakeet-tdt-0.6b-v3-int8');
