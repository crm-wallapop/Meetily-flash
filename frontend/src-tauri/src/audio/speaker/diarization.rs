use anyhow::Result;
use super::types::SpeakerSegment;

/// Port for offline speaker diarization.
pub trait DiarizationPort: Send + Sync {
    fn process(&self, samples: &[f32], sample_rate: u32) -> Result<Vec<SpeakerSegment>>;
}
