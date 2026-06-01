use anyhow::Result;
use super::types::SpeakerSegment;

/// Port for offline speaker diarization.
///
/// `segments` provides the speech regions as `[(start_seconds, end_seconds)]`,
/// typically derived from existing transcript timestamps. When empty, the
/// implementation falls back to energy-based VAD on the raw audio.
pub trait DiarizationPort: Send + Sync {
    fn process(&self, samples: &[f32], sample_rate: u32, segments: &[(f64, f64)]) -> Result<Vec<SpeakerSegment>>;
}
