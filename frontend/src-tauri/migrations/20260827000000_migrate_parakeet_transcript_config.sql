-- Parakeet is no longer a supported transcription engine: its rows carry no
-- token timestamps, so diarization alignment degrades to proportional
-- word-count slicing at speaker changes. Batch transcription resolves to
-- whisper unconditionally (retranscription.rs), so a persisted parakeet
-- config is dead weight that can only confuse. Migrate it to localWhisper
-- with the user's configured Whisper model; the Enhance dialog's
-- availability notice + quality-ordered fallback handle model download
-- state honestly from there.
UPDATE transcript_settings
SET provider = 'localWhisper',
    model = COALESCE(
        (SELECT whisperModel FROM settings WHERE id = '1'),
        'large-v3'
    )
WHERE provider = 'parakeet';
