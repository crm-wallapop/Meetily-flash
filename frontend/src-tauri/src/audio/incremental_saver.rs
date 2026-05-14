use std::path::PathBuf;
use anyhow::{Result, anyhow};
use log::{info, error};
use super::encode::encode_single_audio;
use super::recording_state::AudioChunk;
use serde::{Serialize, Deserialize};

use super::ffmpeg::find_ffmpeg_path;

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

/// Audio recovery status for transcript recovery feature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioRecoveryStatus {
    pub status: String, // "success" | "partial" | "failed" | "none"
    pub chunk_count: u32,
    pub estimated_duration_seconds: f64,
    pub audio_file_path: Option<String>,
    pub message: String,
}

/// Recover audio from checkpoint files
/// This is called by the transcript recovery system to merge audio chunks after a crash
#[tauri::command]
pub async fn recover_audio_from_checkpoints(
    meeting_folder: String,
    _sample_rate: u32
) -> Result<AudioRecoveryStatus, String> {
    info!("Starting audio recovery for folder: {}", meeting_folder);

    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    // Check if checkpoints directory exists
    if !checkpoints_dir.exists() {
        info!("No checkpoints directory found at: {}", checkpoints_dir.display());
        return Ok(AudioRecoveryStatus {
            status: "none".to_string(),
            chunk_count: 0,
            estimated_duration_seconds: 0.0,
            audio_file_path: None,
            message: "No audio checkpoints found".to_string(),
        });
    }

    // Scan for checkpoint files
    let mut checkpoint_files: Vec<_> = std::fs::read_dir(&checkpoints_dir)
        .map_err(|e| format!("Failed to read checkpoints directory: {}", e))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.path().extension().and_then(|s| s.to_str()) == Some("mp4")
        })
        .collect();

    if checkpoint_files.is_empty() {
        info!("No checkpoint files found in: {}", checkpoints_dir.display());
        return Ok(AudioRecoveryStatus {
            status: "none".to_string(),
            chunk_count: 0,
            estimated_duration_seconds: 0.0,
            audio_file_path: None,
            message: "No audio checkpoint files found".to_string(),
        });
    }

    // Sort by filename (audio_chunk_000.mp4, audio_chunk_001.mp4, etc.)
    checkpoint_files.sort_by_key(|entry| entry.path());

    let chunk_count = checkpoint_files.len() as u32;
    let estimated_duration = (chunk_count as f64) * 30.0; // 30 seconds per chunk

    info!("Found {} checkpoint files, estimated duration: {:.2}s", chunk_count, estimated_duration);

    // Create FFmpeg concat file
    let concat_file_path = checkpoints_dir.join("concat_list.txt");
    let mut concat_content = String::new();

    for entry in &checkpoint_files {
        let path = entry.path().canonicalize()
            .map_err(|e| format!("Failed to canonicalize path: {}", e))?;
        concat_content.push_str(&format!("file '{}'\n", path.display()));
    }

    std::fs::write(&concat_file_path, concat_content)
        .map_err(|e| format!("Failed to write concat file: {}", e))?;

    // Run FFmpeg to merge chunks
    let output_path = folder_path.join("audio.mp4");
    let output_path_str = output_path.to_str()
        .ok_or("Invalid output path")?
        .to_string();

    let ffmpeg_path = find_ffmpeg_path()
        .ok_or_else(|| "FFmpeg not found. Please install FFmpeg to recover audio.".to_string())?;
    info!("Using FFmpeg at: {:?}", ffmpeg_path);

    let mut command = std::process::Command::new(ffmpeg_path);

    command.args(&[
        "-f", "concat",
        "-safe", "0",
        "-i", concat_file_path.to_str().unwrap(),
        "-c", "copy",
        "-y", // Overwrite if exists
        &output_path_str
    ]);

    // Hide console window on Windows
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let ffmpeg_result = command.output();

    match ffmpeg_result {
        Ok(output) if output.status.success() => {
            // Clean up concat file
            let _ = std::fs::remove_file(concat_file_path);

            info!("Successfully recovered audio: {}", output_path_str);

            Ok(AudioRecoveryStatus {
                status: "success".to_string(),
                chunk_count,
                estimated_duration_seconds: estimated_duration,
                audio_file_path: Some(output_path_str),
                message: format!("Successfully recovered {} audio chunks", chunk_count),
            })
        }
        Ok(output) => {
            let error = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg recovery failed: {}", error);
            Ok(AudioRecoveryStatus {
                status: "failed".to_string(),
                chunk_count,
                estimated_duration_seconds: estimated_duration,
                audio_file_path: None,
                message: format!("FFmpeg failed: {}", error),
            })
        }
        Err(e) => {
            error!("Failed to run FFmpeg: {}", e);
            Ok(AudioRecoveryStatus {
                status: "failed".to_string(),
                chunk_count,
                estimated_duration_seconds: estimated_duration,
                audio_file_path: None,
                message: format!("Failed to run FFmpeg: {}", e),
            })
        }
    }
}

/// Clean up checkpoint files after successful recording or recovery
/// This command is called by the frontend after successful save to clean up checkpoint files
#[tauri::command]
pub async fn cleanup_checkpoints(meeting_folder: String) -> Result<(), String> {
    info!("Cleaning up checkpoints for folder: {}", meeting_folder);

    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    if checkpoints_dir.exists() {
        std::fs::remove_dir_all(&checkpoints_dir)
            .map_err(|e| format!("Failed to remove checkpoints directory: {}", e))?;
        info!("Successfully cleaned up checkpoints directory");
    } else {
        info!("No checkpoints directory to clean up");
    }

    Ok(())
}

/// Check if a meeting folder has audio checkpoint files
/// Returns true if .checkpoints/ directory exists and contains .mp4 files
#[tauri::command]
pub async fn has_audio_checkpoints(meeting_folder: String) -> Result<bool, String> {
    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    // Check if checkpoints directory exists
    if !checkpoints_dir.exists() {
        return Ok(false);
    }

    // Scan for .mp4 checkpoint files
    let has_mp4_files = std::fs::read_dir(&checkpoints_dir)
        .map_err(|e| format!("Failed to read checkpoints directory: {}", e))?
        .filter_map(|entry| entry.ok())
        .any(|entry| {
            entry.path().extension().and_then(|s| s.to_str()) == Some("mp4")
        });

    Ok(has_mp4_files)
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

        // Add 32 seconds of audio — enough to cross the 30-second checkpoint threshold
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
