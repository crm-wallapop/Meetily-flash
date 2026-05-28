use anyhow::{anyhow, Result};
use std::path::PathBuf;
use tauri::{Emitter, Runtime};
use tauri::AppHandle;

const SEGMENTATION_MODEL_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-reconstruction-models/pyannote-segmentation-3.0.onnx";
const EMBEDDING_MODEL_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-models/3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx";

const SEGMENTATION_FILENAME: &str = "pyannote-segmentation.onnx";
const EMBEDDING_FILENAME: &str = "3dspeaker-embedding.onnx";

fn models_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".meetily-models")
}

pub fn speaker_models_exist() -> bool {
    let dir = models_dir();
    dir.join(EMBEDDING_FILENAME).exists() && dir.join(SEGMENTATION_FILENAME).exists()
}

#[derive(Clone, serde::Serialize)]
struct SpeakerModelDownloadProgress {
    model: String,
    progress: u32,
    downloaded_mb: f64,
    total_mb: f64,
    status: String,
}

#[derive(Clone, serde::Serialize)]
struct SpeakerModelDownloadComplete {
    model: String,
}

#[derive(Clone, serde::Serialize)]
struct SpeakerModelDownloadError {
    model: String,
    error: String,
}

async fn download_file<R: Runtime>(
    app: &AppHandle<R>,
    url: &str,
    filename: &str,
    model_name: &str,
) -> Result<()> {
    let dir = models_dir();
    if !dir.exists() {
        tokio::fs::create_dir_all(&dir).await?;
    }
    let dest = dir.join(filename);

    if dest.exists() {
        let _ = app.emit(
            "speaker-model-download-progress",
            SpeakerModelDownloadProgress {
                model: model_name.to_string(),
                progress: 100,
                downloaded_mb: 0.0,
                total_mb: 0.0,
                status: "completed".to_string(),
            },
        );
        return Ok(());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;

    let response = client.get(url).send().await?;
    let total_size = response.content_length().unwrap_or(0) as f64 / (1024.0 * 1024.0);

    let _ = app.emit(
        "speaker-model-download-progress",
        SpeakerModelDownloadProgress {
            model: model_name.to_string(),
            progress: 0,
            downloaded_mb: 0.0,
            total_mb: total_size,
            status: "downloading".to_string(),
        },
    );

    let temp_path = dir.join(format!("{filename}.tmp"));
    let mut file = tokio::fs::File::create(&temp_path).await?;
    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_reported_pct: u32 = 0;

    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        let pct = if total_size > 0.0 {
            ((downloaded as f64 / (1024.0 * 1024.0)) / total_size * 100.0) as u32
        } else {
            0
        };

        if pct > last_reported_pct + 5 || pct == 100 {
            last_reported_pct = pct;
            let _ = app.emit(
                "speaker-model-download-progress",
                SpeakerModelDownloadProgress {
                    model: model_name.to_string(),
                    progress: pct.min(100),
                    downloaded_mb: downloaded as f64 / (1024.0 * 1024.0),
                    total_mb: total_size,
                    status: "downloading".to_string(),
                },
            );
        }
    }

    file.flush().await?;
    drop(file);
    tokio::fs::rename(&temp_path, &dest).await?;

    let _ = app.emit(
        "speaker-model-download-progress",
        SpeakerModelDownloadProgress {
            model: model_name.to_string(),
            progress: 100,
            downloaded_mb: downloaded as f64 / (1024.0 * 1024.0),
            total_mb: total_size,
            status: "completed".to_string(),
        },
    );

    Ok(())
}

#[tauri::command]
pub async fn download_speaker_models<R: Runtime>(
    app: AppHandle<R>,
) -> Result<(), String> {
    // Download segmentation model
    if let Err(e) = download_file(&app, SEGMENTATION_MODEL_URL, SEGMENTATION_FILENAME, "pyannote-segmentation").await {
        let _ = app.emit(
            "speaker-model-download-error",
            SpeakerModelDownloadError {
                model: "pyannote-segmentation".to_string(),
                error: e.to_string(),
            },
        );
        log::warn!("Failed to download segmentation model: {}", e);
    }

    // Download embedding model
    if let Err(e) = download_file(&app, EMBEDDING_MODEL_URL, EMBEDDING_FILENAME, "3dspeaker-embedding").await {
        let _ = app.emit(
            "speaker-model-download-error",
            SpeakerModelDownloadError {
                model: "3dspeaker-embedding".to_string(),
                error: e.to_string(),
            },
        );
        log::warn!("Failed to download embedding model: {}", e);
    }

    Ok(())
}

#[tauri::command]
pub async fn check_speaker_models_available() -> bool {
    speaker_models_exist()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_dir_is_under_home() {
        let dir = models_dir();
        assert!(dir.to_string_lossy().contains(".meetily-models"));
    }

    #[test]
    fn urls_are_valid_https() {
        assert!(SEGMENTATION_MODEL_URL.starts_with("https://"));
        assert!(EMBEDDING_MODEL_URL.starts_with("https://"));
    }

    #[test]
    fn filenames_match_convention() {
        assert!(SEGMENTATION_FILENAME.ends_with(".onnx"));
        assert!(EMBEDDING_FILENAME.ends_with(".onnx"));
    }
}
