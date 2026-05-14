use std::path::PathBuf;
use anyhow::{Result, anyhow};
use log::info;
use super::encode::encode_single_audio;
use super::recording_state::AudioChunk;

/// Audio saver that accumulates all chunks in memory and encodes to audio.mp4 on finalize.
///
/// The previous implementation wrote intermediate checkpoint files to `.checkpoints/` every 30 s
/// and merged them with FFmpeg concat at the end. That required a pre-created subdirectory,
/// left orphan files on crash, and added an extra FFmpeg pass. The new design buffers samples
/// in RAM and calls `encode_single_audio` once — simpler, no disk intermediates, no merge step.
pub struct IncrementalAudioSaver {
    all_samples: Vec<f32>,
    meeting_folder: PathBuf,
    sample_rate: u32,
}

impl IncrementalAudioSaver {
    /// Create a new audio saver for `meeting_folder`.
    ///
    /// The folder must already exist; no subdirectories are created.
    pub fn new(meeting_folder: PathBuf, sample_rate: u32) -> Result<Self> {
        if !meeting_folder.exists() {
            return Err(anyhow!("Meeting folder does not exist: {}", meeting_folder.display()));
        }
        Ok(Self {
            all_samples: Vec::new(),
            meeting_folder,
            sample_rate,
        })
    }

    /// Append a mixed audio chunk to the in-memory buffer.
    pub fn add_chunk(&mut self, chunk: AudioChunk) -> Result<()> {
        self.all_samples.extend_from_slice(&chunk.data);
        Ok(())
    }

    /// Encode all buffered samples to `audio.mp4` and return its path.
    pub async fn finalize(&mut self) -> Result<PathBuf> {
        info!("Finalizing recording: {} samples ({:.1}s)",
              self.all_samples.len(),
              self.all_samples.len() as f32 / self.sample_rate as f32);

        if self.all_samples.is_empty() {
            return Err(anyhow!("No audio data recorded — cannot save empty recording"));
        }

        let output_path = self.meeting_folder.join("audio.mp4");
        encode_single_audio(
            bytemuck::cast_slice(&self.all_samples),
            self.sample_rate,
            1,
            &output_path,
        )?;

        if !output_path.exists() {
            return Err(anyhow!("audio.mp4 was not created: {}", output_path.display()));
        }

        info!("Saved recording: {}", output_path.display());
        Ok(output_path)
    }

    /// Get the meeting folder path.
    pub fn get_meeting_folder(&self) -> &PathBuf {
        &self.meeting_folder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use super::super::recording_state::DeviceType;

    /// Walk `dir` recursively; return true if any file named `audio_chunk_NNN.mp4` is found.
    fn has_ckpt_mp4_files(dir: &std::path::Path) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else { return false; };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if has_ckpt_mp4_files(&path) {
                    return true;
                }
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("audio_chunk_") && name.ends_with(".mp4") {
                    return true;
                }
            }
        }
        false
    }

    /// RED test (task 2.1) — passes GREEN after task 2.2 removes checkpoint writes.
    ///
    /// Contract: IncrementalAudioSaver must not need a pre-created .checkpoints/
    /// directory, and must not write any audio_chunk_*.mp4 intermediate files while
    /// chunks are being added (streaming directly to audio.mp4 instead).
    #[tokio::test]
    async fn incremental_saver_writes_no_ckpt_files() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Test_Meeting");
        std::fs::create_dir_all(&meeting_folder).unwrap();
        // Intentionally do NOT create .checkpoints/ — the new implementation must not need it.

        let mut saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000)
            .expect("IncrementalAudioSaver::new must succeed without a .checkpoints directory (task 2.2)");

        // Add 32 seconds of audio — enough to cross the old 30-second checkpoint threshold
        for i in 0u64..64 {
            let chunk = AudioChunk {
                data: vec![0.1f32; 24000],  // 0.5 s at 48 kHz
                sample_rate: 48000,
                timestamp: i as f64 * 0.5,
                chunk_id: i,
                device_type: DeviceType::Microphone,
            };
            saver.add_chunk(chunk).unwrap();
        }

        assert!(
            !meeting_folder.join(".checkpoints").exists(),
            "IncrementalAudioSaver must not create a .checkpoints directory (task 2.2)"
        );
        assert!(
            !has_ckpt_mp4_files(&meeting_folder),
            "IncrementalAudioSaver must not write audio_chunk_*.mp4 checkpoint files (task 2.2)"
        );
    }

    #[tokio::test]
    async fn test_checkpoint_creation() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Test_Meeting");
        std::fs::create_dir_all(&meeting_folder).unwrap();

        let mut saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();

        // Add 60 seconds of audio
        for i in 0u64..120 {
            let chunk = AudioChunk {
                data: vec![0.5f32; 24000],  // 0.5 s at 48 kHz
                sample_rate: 48000,
                timestamp: i as f64 * 0.5,
                chunk_id: i,
                device_type: DeviceType::Microphone,
            };
            saver.add_chunk(chunk).unwrap();
        }

        // No intermediate checkpoint files should exist
        assert!(!meeting_folder.join(".checkpoints").exists());

        // Finalize should produce audio.mp4 directly
        let final_path = saver.finalize().await.unwrap();
        assert!(final_path.exists());
        assert_eq!(final_path.file_name().unwrap(), "audio.mp4");
    }

    #[tokio::test]
    async fn test_empty_recording() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Empty_Test");
        std::fs::create_dir_all(&meeting_folder).unwrap();

        let mut saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();

        let result = saver.finalize().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No audio data"));
    }
}
