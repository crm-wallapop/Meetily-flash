-- Scratch table for per-segment retranscription checkpoints (retranscription-checkpoint).
-- Written after each transcribed segment; loaded on resume to skip already-transcribed
-- segments; deleted on completion and on cancel. Never exposed to the UI or the final
-- transcripts table — purely a resume-fast scratch layer.
CREATE TABLE IF NOT EXISTS retranscription_checkpoints (
    meeting_id TEXT NOT NULL,
    segment_index INTEGER NOT NULL,
    text TEXT NOT NULL,
    start_ms REAL NOT NULL,
    end_ms REAL NOT NULL,
    confidence REAL NOT NULL,
    PRIMARY KEY (meeting_id, segment_index)
);
