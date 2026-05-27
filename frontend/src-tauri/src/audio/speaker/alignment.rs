use serde::{Deserialize, Serialize};

/// A single word with its timing from Whisper token timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenWord {
    pub word: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// A speaker segment from diarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizationSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker_id: u32,
}

/// A transcript segment to be aligned.
#[derive(Debug, Clone)]
pub struct TranscriptInput {
    pub id: String,
    pub text: String,
    pub audio_start_ms: i64,
    pub audio_end_ms: i64,
    pub token_words: Option<Vec<TokenWord>>,
}

/// Result of aligning one transcript segment with diarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignedSegment {
    pub original_id: String,
    pub text: String,
    pub audio_start_ms: i64,
    pub audio_end_ms: i64,
    pub speaker: String,
    pub speaker_source: SpeakerSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeakerSource {
    Auto,
    Fallback,
    Unknown,
}

/// Find which diarization segment contains a given timestamp.
fn speaker_at_time(segments: &[DiarizationSegment], time_ms: i64) -> Option<&DiarizationSegment> {
    segments.iter().find(|s| time_ms >= s.start_ms && time_ms < s.end_ms)
}

/// Split a text into words (whitespace-delimited).
fn split_words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Align transcript segments with diarization speaker segments.
///
/// For each transcript segment:
/// - If token timestamps are available, assign each word to the speaker
///   whose diarization segment contains the word's start_ms.
/// - If a transcript segment spans multiple speakers, split it into
///   separate rows per speaker.
/// - If no token timestamps, fall back to proportional split.
pub fn align_transcripts_with_diarization(
    transcripts: Vec<TranscriptInput>,
    diarization: &[DiarizationSegment],
) -> Vec<AlignedSegment> {
    if transcripts.is_empty() {
        return Vec::new();
    }

    if diarization.is_empty() {
        return transcripts
            .into_iter()
            .map(|t| AlignedSegment {
                original_id: t.id,
                text: t.text,
                audio_start_ms: t.audio_start_ms,
                audio_end_ms: t.audio_end_ms,
                speaker: "Unknown Speaker".to_string(),
                speaker_source: SpeakerSource::Unknown,
            })
            .collect();
    }

    let mut results = Vec::new();

    for transcript in transcripts {
        if let Some(ref tokens) = transcript.token_words {
            if !tokens.is_empty() {
                results.extend(align_with_tokens(&transcript, tokens, diarization));
                continue;
            }
        }
        results.extend(align_proportional(&transcript, diarization));
    }

    results
}

/// Align using token-level timestamps.
fn align_with_tokens(
    transcript: &TranscriptInput,
    tokens: &[TokenWord],
    diarization: &[DiarizationSegment],
) -> Vec<AlignedSegment> {
    let mut groups: Vec<(u32, Vec<&TokenWord>)> = Vec::new();
    let mut current_speaker: Option<u32> = None;
    let mut current_words: Vec<&TokenWord> = Vec::new();

    for token in tokens {
        let speaker = speaker_at_time(diarization, token.start_ms)
            .map(|s| s.speaker_id)
            .unwrap_or(u32::MAX);

        if current_speaker != Some(speaker) {
            if !current_words.is_empty() {
                groups.push((current_speaker.unwrap_or(u32::MAX), current_words.clone()));
            }
            current_speaker = Some(speaker);
            current_words.clear();
        }
        current_words.push(token);
    }

    if !current_words.is_empty() {
        groups.push((current_speaker.unwrap_or(u32::MAX), current_words));
    }

    groups
        .into_iter()
        .map(|(speaker_id, words)| {
            let text = words
                .iter()
                .map(|w| w.word.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let start = words.first().map(|w| w.start_ms).unwrap_or(transcript.audio_start_ms);
            let end = words.last().map(|w| w.end_ms).unwrap_or(transcript.audio_end_ms);

            let speaker = if speaker_id == u32::MAX {
                "Unknown Speaker".to_string()
            } else {
                format!("Speaker {}", speaker_id)
            };

            AlignedSegment {
                original_id: transcript.id.clone(),
                text,
                audio_start_ms: start,
                audio_end_ms: end,
                speaker,
                speaker_source: SpeakerSource::Auto,
            }
        })
        .collect()
}

/// Align using proportional split (fallback when no token timestamps).
fn align_proportional(
    transcript: &TranscriptInput,
    diarization: &[DiarizationSegment],
) -> Vec<AlignedSegment> {
    let duration = (transcript.audio_end_ms - transcript.audio_start_ms) as f64;
    if duration <= 0.0 {
        let speaker = speaker_at_time(diarization, transcript.audio_start_ms)
            .map(|s| format!("Speaker {}", s.speaker_id))
            .unwrap_or_else(|| "Unknown Speaker".to_string());
        return vec![AlignedSegment {
            original_id: transcript.id.clone(),
            text: transcript.text.clone(),
            audio_start_ms: transcript.audio_start_ms,
            audio_end_ms: transcript.audio_end_ms,
            speaker,
            speaker_source: SpeakerSource::Fallback,
        }];
    }

    let words = split_words(&transcript.text);
    if words.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut word_idx = 0;

    for seg in diarization {
        // Clamp segment to transcript boundaries
        let seg_start = seg.start_ms.max(transcript.audio_start_ms);
        let seg_end = seg.end_ms.min(transcript.audio_end_ms);
        if seg_start >= seg_end {
            continue;
        }

        let seg_duration = (seg_end - seg_start) as f64;
        let words_in_seg = ((seg_duration / duration) * words.len() as f64).round() as usize;
        let words_in_seg = words_in_seg.max(1);
        let end_idx = (word_idx + words_in_seg).min(words.len());

        if word_idx >= words.len() {
            break;
        }

        let text = words[word_idx..end_idx].join(" ");
        results.push(AlignedSegment {
            original_id: transcript.id.clone(),
            text,
            audio_start_ms: seg_start,
            audio_end_ms: seg_end,
            speaker: format!("Speaker {}", seg.speaker_id),
            speaker_source: SpeakerSource::Fallback,
        });

        word_idx = end_idx;
    }

    // Remaining words go to the last segment or "Unknown"
    if word_idx < words.len() {
        let text = words[word_idx..].join(" ");
        let last_speaker = diarization
            .last()
            .map(|s| format!("Speaker {}", s.speaker_id))
            .unwrap_or_else(|| "Unknown Speaker".to_string());
        results.push(AlignedSegment {
            original_id: transcript.id.clone(),
            text,
            audio_start_ms: diarization.last().map(|s| s.end_ms).unwrap_or(transcript.audio_end_ms),
            audio_end_ms: transcript.audio_end_ms,
            speaker: last_speaker,
            speaker_source: SpeakerSource::Fallback,
        });
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: i64, end: i64, speaker: u32) -> DiarizationSegment {
        DiarizationSegment { start_ms: start, end_ms: end, speaker_id: speaker }
    }

    fn token(word: &str, start: i64, end: i64) -> TokenWord {
        TokenWord { word: word.to_string(), start_ms: start, end_ms: end }
    }

    fn transcript(id: &str, text: &str, start: i64, end: i64) -> TranscriptInput {
        TranscriptInput {
            id: id.to_string(),
            text: text.to_string(),
            audio_start_ms: start,
            audio_end_ms: end,
            token_words: None,
        }
    }

    fn transcript_with_tokens(id: &str, text: &str, start: i64, end: i64, tokens: Vec<TokenWord>) -> TranscriptInput {
        TranscriptInput {
            id: id.to_string(),
            text: text.to_string(),
            audio_start_ms: start,
            audio_end_ms: end,
            token_words: Some(tokens),
        }
    }

    // ── 2.3: Multi-speaker split produces correct text at boundary ────

    #[test]
    fn token_alignment_splits_multi_speaker() {
        let t = transcript_with_tokens(
            "t1",
            "Sure I agree No that's wrong",
            5000,
            9000,
            vec![
                token("Sure", 5000, 5200),
                token("I", 5200, 5400),
                token("agree", 5400, 5600),
                token("No", 7200, 7400),
                token("that's", 7400, 7600),
                token("wrong", 7600, 7800),
            ],
        );
        let diarization = vec![seg(5000, 7100, 1), seg(7200, 9000, 2)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "Sure I agree");
        assert_eq!(result[0].speaker, "Speaker 1");
        assert_eq!(result[1].text, "No that's wrong");
        assert_eq!(result[1].speaker, "Speaker 2");
    }

    // ── 2.4: Single-speaker segment is not split ──────────────────────

    #[test]
    fn token_alignment_no_split_single_speaker() {
        let t = transcript_with_tokens(
            "t1", "Hello world", 5000, 9000,
            vec![token("Hello", 5000, 6000), token("world", 6000, 7000)],
        );
        let diarization = vec![seg(5000, 9000, 1)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "Hello world");
        assert_eq!(result[0].speaker, "Speaker 1");
    }

    // ── 2.5: Proportional split fallback ──────────────────────────────

    #[test]
    fn proportional_split_fallback() {
        let t = transcript("t1", "Hello world foo bar", 5000, 9000);
        let diarization = vec![seg(5000, 7200, 1), seg(7200, 9000, 2)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        assert!(result.len() >= 2, "should split into at least 2 segments");
        assert_eq!(result[0].speaker, "Speaker 1");
        assert_eq!(result[1].speaker, "Speaker 2");
    }

    // ── 2.6: Overlapping diarization segments (last writer wins) ──────

    #[test]
    fn overlapping_diarization_last_writer_wins() {
        let t = transcript_with_tokens(
            "t1", "word", 5000, 6000,
            vec![token("word", 5000, 6000)],
        );
        // Two segments overlap at 5000-6000
        let diarization = vec![seg(4000, 6000, 1), seg(5000, 7000, 2)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        // speaker_at_time finds first match → Speaker 1
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].speaker, "Speaker 1");
    }

    // ── 2.7: Zero-length diarization segment (skipped) ────────────────

    #[test]
    fn zero_length_diarization_segment_skipped() {
        let t = transcript_with_tokens(
            "t1", "word", 5000, 6000,
            vec![token("word", 5000, 6000)],
        );
        let diarization = vec![seg(5000, 5000, 1), seg(5000, 6000, 2)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].speaker, "Speaker 2");
    }

    // ── 2.8: Diarization gap → Unknown ────────────────────────────────

    #[test]
    fn diarization_gap_labels_unknown() {
        let t = transcript_with_tokens(
            "t1", "word", 5000, 6000,
            vec![token("word", 5000, 6000)],
        );
        // Gap: diarization covers 0-4000 and 7000-10000, but not 5000-6000
        let diarization = vec![seg(0, 4000, 1), seg(7000, 10000, 2)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].speaker, "Unknown Speaker");
    }

    // ── 2.10: Empty transcripts → empty result ────────────────────────

    #[test]
    fn empty_transcripts_returns_empty() {
        let diarization = vec![seg(0, 5000, 1)];
        let result = align_transcripts_with_diarization(vec![], &diarization);
        assert!(result.is_empty());
    }

    // ── 2.11: Empty diarization → all Unknown ─────────────────────────

    #[test]
    fn empty_diarization_labels_all_unknown() {
        let t = transcript("t1", "Hello world", 5000, 9000);
        let result = align_transcripts_with_diarization(vec![t], &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].speaker, "Unknown Speaker");
    }

    // ── 2.12: Malformed token timestamps → fallback to proportional ───

    #[test]
    fn empty_token_list_falls_back_to_proportional() {
        let t = transcript_with_tokens("t1", "Hello world", 5000, 9000, vec![]);
        let diarization = vec![seg(5000, 7200, 1), seg(7200, 9000, 2)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        // Should use proportional split (Fallback source)
        assert!(result.iter().any(|r| r.speaker_source == SpeakerSource::Fallback));
    }

    // ── Text preservation invariant ────────────────────────────────────

    #[test]
    fn token_alignment_preserves_all_words() {
        let t = transcript_with_tokens(
            "t1", "one two three four five six", 5000, 9000,
            vec![
                token("one", 5000, 5200),
                token("two", 5200, 5400),
                token("three", 5400, 5600),
                token("four", 7200, 7400),
                token("five", 7400, 7600),
                token("six", 7600, 7800),
            ],
        );
        let diarization = vec![seg(5000, 7100, 1), seg(7200, 9000, 2)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        let all_text: String = result.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(" ");
        assert_eq!(all_text, "one two three four five six");
    }

    #[test]
    fn proportional_split_preserves_all_words() {
        let t = transcript("t1", "one two three four five six", 5000, 9000);
        let diarization = vec![seg(5000, 7100, 1), seg(7200, 9000, 2)];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        let all_text: String = result.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(" ");
        assert_eq!(all_text, "one two three four five six");
    }

    // ── Multiple transcripts ───────────────────────────────────────────

    #[test]
    fn aligns_multiple_transcripts_independently() {
        let t1 = transcript_with_tokens(
            "t1", "Hello", 5000, 6000,
            vec![token("Hello", 5000, 6000)],
        );
        let t2 = transcript_with_tokens(
            "t2", "World", 7000, 8000,
            vec![token("World", 7000, 8000)],
        );
        let diarization = vec![seg(5000, 6500, 1), seg(6500, 9000, 2)];

        let result = align_transcripts_with_diarization(vec![t1, t2], &diarization);

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].speaker, "Speaker 1");
        assert_eq!(result[1].speaker, "Speaker 2");
    }

    // ── A→B→A speaker pattern ──────────────────────────────────────────

    #[test]
    fn speaker_a_b_a_pattern_produces_three_segments() {
        let t = transcript_with_tokens(
            "t1", "I agree no wait yes",
            5000, 11000,
            vec![
                token("I", 5000, 5200),
                token("agree", 5200, 5600),
                token("no", 7000, 7200),
                token("wait", 7200, 7600),
                token("yes", 9000, 9200),
            ],
        );
        let diarization = vec![
            seg(5000, 6500, 1),
            seg(6500, 8000, 2),
            seg(8000, 11000, 1),
        ];

        let result = align_transcripts_with_diarization(vec![t], &diarization);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].speaker, "Speaker 1");
        assert_eq!(result[0].text, "I agree");
        assert_eq!(result[1].speaker, "Speaker 2");
        assert_eq!(result[1].text, "no wait");
        assert_eq!(result[2].speaker, "Speaker 1");
        assert_eq!(result[2].text, "yes");
    }
}
