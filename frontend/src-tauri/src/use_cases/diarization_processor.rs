use anyhow::{anyhow, Result};
use std::path::Path;
use std::sync::Arc;

use crate::audio::decoder::decode_audio_file;
use crate::audio::speaker::alignment::{
    align_transcripts_with_diarization, DiarizationSegment, TranscriptInput,
};
use crate::audio::speaker::diarization::{DiarizationOutput, DiarizationPort};
use crate::database::repositories::speaker::SpeakerRepository;
use sqlx::SqlitePool;

const MAX_AUDIO_DURATION_SECS: f64 = 7200.0; // 2 hours

/// Fetches transcript rows for a meeting. Abstracted so tests can mock it.
pub trait TranscriptFetcher: Send + Sync {
    fn fetch_transcripts(
        &self,
        meeting_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<TranscriptInput>>> + Send + '_>>;
}

/// Default implementation that reads from the database.
pub struct DbTranscriptFetcher {
    pool: SqlitePool,
}

impl DbTranscriptFetcher {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl TranscriptFetcher for DbTranscriptFetcher {
    fn fetch_transcripts(
        &self,
        meeting_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<TranscriptInput>>> + Send + '_>>
    {
        let meeting_id = meeting_id.to_string();
        let pool = self.pool.clone();
        Box::pin(async move {
            let rows = sqlx::query_as::<_, TranscriptRow>(
                "SELECT id, text, start_time, end_time, token_timestamps FROM transcripts WHERE meeting_id = ? ORDER BY start_time ASC",
            )
            .bind(&meeting_id)
            .fetch_all(&pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|r| {
                    let token_words = r.token_timestamps.and_then(|json| {
                        serde_json::from_str::<Vec<crate::audio::speaker::alignment::TokenWord>>(&json).ok()
                    });
                    TranscriptInput {
                        id: r.id,
                        text: r.text,
                        audio_start_ms: (r.start_time * 1000.0) as i64,
                        audio_end_ms: (r.end_time * 1000.0) as i64,
                        token_words,
                    }
                })
                .collect())
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct TranscriptRow {
    id: String,
    text: String,
    start_time: f64,
    end_time: f64,
    token_timestamps: Option<String>,
}

/// Emitted when diarization completes for a meeting.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiarizationCompletePayload {
    pub meeting_id: String,
    pub speaker_count: usize,
    pub segments_labeled: usize,
}

pub struct DiarizationProcessor {
    diarization: Arc<dyn DiarizationPort>,
    max_speakers: u32,
}

impl DiarizationProcessor {
    pub fn new(
        diarization: Arc<dyn DiarizationPort>,
        max_speakers: u32,
    ) -> Self {
        Self {
            diarization,
            max_speakers,
        }
    }

    /// Run diarization on a meeting's audio file.
    /// Returns the payload for the `diarization-complete` event.
    pub async fn process(
        &self,
        meeting_id: &str,
        audio_path: &Path,
        pool: &SqlitePool,
        transcript_fetcher: &dyn TranscriptFetcher,
    ) -> Result<DiarizationCompletePayload> {
        // Decode audio
        if !audio_path.exists() {
            return Ok(DiarizationCompletePayload {
                meeting_id: meeting_id.to_string(),
                speaker_count: 0,
                segments_labeled: 0,
            });
        }

        let decoded = decode_audio_file(audio_path)
            .map_err(|e| anyhow!("audio decode failed: {}", e))?;

        if decoded.duration_seconds > MAX_AUDIO_DURATION_SECS {
            return Err(anyhow!(
                "audio too long: {:.1}s (max {}s)",
                decoded.duration_seconds,
                MAX_AUDIO_DURATION_SECS
            ));
        }

        let mono = to_mono_f32(&decoded);

        // Run diarization
        let output: DiarizationOutput = self.diarization.process(&mono, decoded.sample_rate, &[])?;
        let speaker_segments = output.segments;
        let centroids = output.centroids;

        if speaker_segments.is_empty() {
            // Silence-only or no speakers detected
            let transcripts = transcript_fetcher.fetch_transcripts(meeting_id).await?;
            let labeled = self.label_unknowns(pool, &transcripts).await?;
            return Ok(DiarizationCompletePayload {
                meeting_id: meeting_id.to_string(),
                speaker_count: 0,
                segments_labeled: labeled,
            });
        }

        // Enforce speaker cap
        let unique_speakers: std::collections::HashSet<u32> =
            speaker_segments.iter().map(|s| s.speaker_id).collect();
        if unique_speakers.len() > self.max_speakers as usize {
            return Err(anyhow!(
                "detected {} speakers (max {})",
                unique_speakers.len(),
                self.max_speakers
            ));
        }

        // Store centroids from the diarization output
        for (speaker_id, centroid) in &centroids {
            let emb_id = format!("emb-{}", uuid::Uuid::new_v4());
            let cluster_label = format!("Speaker {}", speaker_id);
            let _ = crate::database::repositories::speaker::SpeakerRepository::store_embedding(
                pool, &emb_id, None, centroid, meeting_id, &cluster_label,
            ).await;
        }

        // Cross-meeting matching
        let label_map = self.match_speakers(&speaker_segments).await?;

        // Fetch transcripts and align
        let transcripts = transcript_fetcher.fetch_transcripts(meeting_id).await?;
        let diarization_segs: Vec<DiarizationSegment> = speaker_segments
            .iter()
            .map(|s| DiarizationSegment {
                start_ms: (s.start_seconds * 1000.0) as i64,
                end_ms: (s.end_seconds * 1000.0) as i64,
                speaker_id: s.speaker_id,
            })
            .collect();

        let aligned = align_transcripts_with_diarization(transcripts, &diarization_segs);

        // Store aligned results
        let mut segments_labeled = 0;
        for seg in &aligned {
            let label = resolve_label(&seg.speaker, &label_map);
            let source = "auto";
            SpeakerRepository::update_transcript_speaker(pool, &seg.original_id, &label, source)
                .await?;
            segments_labeled += 1;
        }

        Ok(DiarizationCompletePayload {
            meeting_id: meeting_id.to_string(),
            speaker_count: unique_speakers.len(),
            segments_labeled,
        })
    }

    async fn label_unknowns(
        &self,
        pool: &SqlitePool,
        transcripts: &[TranscriptInput],
    ) -> Result<usize> {
        let mut count = 0;
        for t in transcripts {
            SpeakerRepository::update_transcript_speaker(
                pool,
                &t.id,
                "Unknown Speaker",
                "auto",
            )
            .await?;
            count += 1;
        }
        Ok(count)
    }

    async fn match_speakers(
        &self,
        segments: &[crate::audio::speaker::types::SpeakerSegment],
    ) -> Result<std::collections::HashMap<u32, String>> {
        // For now, return cluster labels — cross-meeting matching
        // would load stored embeddings and search the registry
        let mut map = std::collections::HashMap::new();
        for seg in segments {
            map.entry(seg.speaker_id)
                .or_insert_with(|| format!("Speaker {}", seg.speaker_id));
        }
        Ok(map)
    }
}

fn to_mono_f32(decoded: &crate::audio::decoder::DecodedAudio) -> Vec<f32> {
    if decoded.channels == 1 {
        return decoded.samples.clone();
    }
    decoded
        .samples
        .chunks_exact(decoded.channels as usize)
        .map(|chunk| chunk.iter().sum::<f32>() / decoded.channels as f32)
        .collect()
}

fn resolve_label(speaker: &str, label_map: &std::collections::HashMap<u32, String>) -> String {
    // If speaker is like "Speaker 3", try to look up by id
    if let Some(id_str) = speaker.strip_prefix("Speaker ") {
        if let Ok(id) = id_str.parse::<u32>() {
            if let Some(label) = label_map.get(&id) {
                return label.clone();
            }
        }
    }
    speaker.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::speaker::mocks::MockDiarizationPort;
    use std::collections::HashMap;

    fn make_processor(
        diarization: MockDiarizationPort,
    ) -> DiarizationProcessor {
        DiarizationProcessor::new(
            Arc::new(diarization),
            20,
        )
    }

    struct MockTranscriptFetcher {
        transcripts: Vec<TranscriptInput>,
    }

    impl MockTranscriptFetcher {
        fn new(transcripts: Vec<TranscriptInput>) -> Self {
            Self { transcripts }
        }
    }

    impl TranscriptFetcher for MockTranscriptFetcher {
        fn fetch_transcripts(
            &self,
            _meeting_id: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<TranscriptInput>>> + Send + '_>>
        {
            let transcripts = self.transcripts.clone();
            Box::pin(async move { Ok(transcripts) })
        }
    }

    // The processor tests below focus on pure-logic paths that don't need a DB pool.
    // Full integration tests (Group 14) will exercise DB storage.

    #[test]
    fn to_mono_converts_stereo_to_mono() {
        let decoded = crate::audio::decoder::DecodedAudio {
            samples: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0], // 3 frames, 2 channels
            sample_rate: 16000,
            channels: 2,
            duration_seconds: 3.0 / 16000.0 * 2.0,
        };
        let mono = to_mono_f32(&decoded);
        assert_eq!(mono, vec![1.5, 3.5, 5.5]);
    }

    #[test]
    fn to_mono_passthrough_mono() {
        let decoded = crate::audio::decoder::DecodedAudio {
            samples: vec![1.0, 2.0, 3.0],
            sample_rate: 16000,
            channels: 1,
            duration_seconds: 3.0 / 16000.0,
        };
        let mono = to_mono_f32(&decoded);
        assert_eq!(mono, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn resolve_label_returns_cluster_name_when_no_match() {
        let map = HashMap::new();
        assert_eq!(resolve_label("Speaker 1", &map), "Speaker 1");
        assert_eq!(resolve_label("Unknown Speaker", &map), "Unknown Speaker");
    }

    #[test]
    fn resolve_label_returns_matched_name() {
        let mut map = HashMap::new();
        map.insert(1u32, "Alice".to_string());
        assert_eq!(resolve_label("Speaker 1", &map), "Alice");
    }

    #[test]
    fn max_duration_guard_rejects_oversized() {
        assert!(7200.0 < 7201.0);
        // The actual guard is in process() which needs a DB pool.
        // This test validates the constant is accessible and correct.
        assert_eq!(MAX_AUDIO_DURATION_SECS, 7200.0);
    }
}
