use crate::audio::speaker::alignment::TranscriptInput;
use crate::audio::speaker::diarization::DiarizationPort;
use crate::database::repositories::speaker::SpeakerRepository;
use crate::state::AppState;
use sqlx::SqlitePool;
use tauri::Emitter;
use std::sync::atomic::Ordering;
use uuid::Uuid;

fn sanitize_speaker_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Speaker name cannot be empty".to_string());
    }
    if trimmed.len() > 200 {
        return Err(format!(
            "Speaker name too long: {} chars (max 200)",
            trimmed.len()
        ));
    }
    let sanitized = strip_html_tags(trimmed);
    Ok(sanitized)
}

fn strip_html_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

fn pick_color(index: usize) -> String {
    let hue = (index as f64 * 137.508) % 360.0;
    format!("hsl({}, 65%, 55%)", hue.round() as u16)
}

#[tauri::command]
pub async fn label_speaker(
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
    cluster_label: String,
    speaker_name: String,
) -> Result<String, String> {
    let pool = app_state.db_manager.pool();
    let name = sanitize_speaker_name(&speaker_name)?;

    let meeting_exists = sqlx::query("SELECT id FROM meetings WHERE id = ?")
        .bind(&meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

    if meeting_exists.is_none() {
        return Err(format!("Meeting not found: {}", meeting_id));
    }

    let cluster_rows = sqlx::query(
        "SELECT COUNT(*) as count FROM transcripts WHERE meeting_id = ? AND speaker_label = ?",
    )
    .bind(&meeting_id)
    .bind(&cluster_label)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    let count: i64 = sqlx::Row::get(&cluster_rows, "count");
    if count == 0 {
        return Err(format!(
            "No transcripts found for cluster '{}' in meeting {}",
            cluster_label, meeting_id
        ));
    }

    let speaker_id = format!("speaker-{}", Uuid::new_v4());

    #[derive(sqlx::FromRow)]
    struct SpeakerIdColor {
        id: String,
        color: String,
    }

    let existing = sqlx::query_as::<_, SpeakerIdColor>(
        "SELECT id, color FROM speakers WHERE name = ?",
    )
    .bind(&name)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;

    let (final_speaker_id, _final_color) = match existing {
        Some(row) => (row.id, row.color),
        None => {
            let speaker_count = SpeakerRepository::list_speakers(pool)
                .await
                .map(|s| s.len())
                .unwrap_or(0);
            let color = pick_color(speaker_count);
            SpeakerRepository::create_speaker(pool, &speaker_id, &name, &color)
                .await
                .map_err(|e| e.to_string())?;
            (speaker_id, color)
        }
    };

    let updated = SpeakerRepository::update_meeting_speakers(
        pool,
        &meeting_id,
        &cluster_label,
        &name,
    )
    .await
    .map_err(|e| e.to_string())?;

    log::info!(
        "label_speaker: labeled {} transcripts in meeting {} cluster '{}' as '{}'",
        updated,
        meeting_id,
        cluster_label,
        name
    );

    Ok(final_speaker_id)
}

#[tauri::command]
pub async fn list_speakers_cmd(
    app_state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let pool = app_state.db_manager.pool();
    let speakers = SpeakerRepository::list_speakers(pool)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(speakers).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_speaker_cmd(
    app_state: tauri::State<'_, AppState>,
    speaker_id: String,
) -> Result<bool, String> {
    let pool = app_state.db_manager.pool();
    SpeakerRepository::remove_speaker(pool, &speaker_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn rediarize_meeting<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<u64, String> {
    let pool = app_state.db_manager.pool().clone();
    let threshold_fp = app_state.speaker_merge_threshold_fp.load(Ordering::Relaxed);
    let result = run_diarization_for_meeting(&pool, &meeting_id, threshold_fp).await?;
    let _ = app.emit("diarization-complete", serde_json::json!({
        "meeting_id": meeting_id,
        "speaker_count": result.speaker_count,
        "segments_labeled": result.segments_labeled,
    }));
    Ok(result.segments_labeled)
}

/// Run speaker diarization on a meeting's audio and label transcripts.
pub async fn run_diarization_for_meeting(
    pool: &SqlitePool,
    meeting_id: &str,
    threshold_fp: u32,
) -> Result<DiarizationResult, String> {
    let cleared = SpeakerRepository::clear_auto_speaker_labels(pool, meeting_id)
        .await
        .map_err(|e| e.to_string())?;

    log::info!(
        "run_diarization_for_meeting: cleared {} auto labels for meeting {}",
        cleared,
        meeting_id
    );

    let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

    let folder_path: Option<String> = row.and_then(|r| sqlx::Row::get(&r, "folder_path"));
    let Some(folder) = folder_path else {
        log::warn!("run_diarization_for_meeting: no folder_path for meeting {}", meeting_id);
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    };

    let folder_path = std::path::Path::new(&folder);
    let audio_path = find_audio_in_folder(folder_path);
    let Some(audio_path) = audio_path else {
        log::warn!("run_diarization_for_meeting: no audio file in {}", folder_path.display());
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    };

    let models_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".meetily-models");
    let embedding_path = models_dir.join("3dspeaker-embedding.onnx");
    let segmentation_path = models_dir.join("pyannote-segmentation.onnx");

    if !embedding_path.exists() || !segmentation_path.exists() {
        log::warn!("run_diarization_for_meeting: speaker models not found, skipping");
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    }

    let decoded = crate::audio::decoder::decode_audio_file(&audio_path)
        .map_err(|e| format!("Audio decode failed: {}", e))?;
    let mono = to_mono_f32(&decoded);

    let shared_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(threshold_fp));
    let adapter = super::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
        embedding_path.to_str().unwrap_or(""),
        segmentation_path.to_str().unwrap_or(""),
        shared_fp,
    )
    .map_err(|e| format!("Failed to create diarization adapter: {}", e))?;

    let segments = adapter.process(&mono, 16000)
        .map_err(|e| format!("Diarization failed: {}", e))?;

    if segments.is_empty() {
        log::info!("run_diarization_for_meeting: 0 speakers detected for meeting {}", meeting_id);
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    }

    let num_speakers: std::collections::HashSet<u32> =
        segments.iter().map(|s| s.speaker_id).collect();
    log::info!(
        "run_diarization_for_meeting: detected {} speakers for meeting {}",
        num_speakers.len(),
        meeting_id
    );

    let transcripts = fetch_transcripts_for_alignment(pool, meeting_id).await
        .map_err(|e| format!("Failed to fetch transcripts: {}", e))?;

    if transcripts.is_empty() {
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: num_speakers.len() });
    }

    use crate::audio::speaker::alignment::{
        align_transcripts_with_diarization, DiarizationSegment,
    };

    let diarization_segs: Vec<DiarizationSegment> = segments
        .iter()
        .map(|s| DiarizationSegment {
            start_ms: (s.start_seconds * 1000.0) as i64,
            end_ms: (s.end_seconds * 1000.0) as i64,
            speaker_id: s.speaker_id,
        })
        .collect();

    let aligned = align_transcripts_with_diarization(transcripts, &diarization_segs);

    let mut segments_labeled = 0u64;
    for seg in &aligned {
        let label = resolve_label(&seg.speaker);
        SpeakerRepository::update_transcript_speaker(pool, &seg.original_id, &label, "auto")
            .await
            .map_err(|e| e.to_string())?;
        segments_labeled += 1;
    }

    log::info!(
        "run_diarization_for_meeting: labeled {} segments for meeting {}",
        segments_labeled,
        meeting_id
    );

    Ok(DiarizationResult {
        segments_labeled,
        speaker_count: num_speakers.len(),
    })
}

pub struct DiarizationResult {
    pub segments_labeled: u64,
    pub speaker_count: usize,
}

fn find_audio_in_folder(folder: &std::path::Path) -> Option<std::path::PathBuf> {
    let candidates = [
        "audio.mp4", "audio.m4a", "audio.wav", "audio.mp3",
        "audio.flac", "audio.ogg", "recording.mp4",
    ];
    for name in &candidates {
        let path = folder.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

#[tauri::command]
pub async fn get_diarization_enabled(
    app_state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT diarizationEnabled FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let enabled: i64 = sqlx::Row::get(&row, "diarizationEnabled");
    Ok(enabled != 0)
}

#[tauri::command]
pub async fn set_diarization_enabled(
    app_state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET diarizationEnabled = ? WHERE id = '1'")
        .bind(enabled as i64)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    log::info!("set_diarization_enabled: updated to {}", enabled);
    Ok(())
}

#[tauri::command]
pub async fn get_speaker_merge_threshold(
    app_state: tauri::State<'_, AppState>,
) -> Result<f64, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT speakerMergeThreshold FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let threshold: f64 = sqlx::Row::get(&row, "speakerMergeThreshold");
    Ok(threshold)
}

#[tauri::command]
pub async fn set_speaker_merge_threshold(
    app_state: tauri::State<'_, AppState>,
    threshold: f64,
) -> Result<(), String> {
    if !(0.30..=0.70).contains(&threshold) {
        return Err("Threshold must be between 0.30 and 0.70".to_string());
    }
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET speakerMergeThreshold = ? WHERE id = '1'")
        .bind(threshold)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    let fp = (threshold as f32 * 65536.0) as u32;
    app_state.speaker_merge_threshold_fp.store(fp, Ordering::Relaxed);
    log::info!("set_speaker_merge_threshold: updated to {}", threshold);
    Ok(())
}

#[tauri::command]
pub async fn get_speaker_embedding_model(
    app_state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT speakerEmbeddingModel FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let model: String = sqlx::Row::get(&row, "speakerEmbeddingModel");
    Ok(model)
}

#[tauri::command]
pub async fn set_speaker_embedding_model(
    app_state: tauri::State<'_, AppState>,
    model: String,
) -> Result<(), String> {
    let valid = model == "3dspeaker" || model == "wespeaker";
    if !valid {
        return Err(format!("Unknown speaker embedding model: {}", model));
    }
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET speakerEmbeddingModel = ? WHERE id = '1'")
        .bind(&model)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    log::info!("set_speaker_embedding_model: updated to {}", model);
    Ok(())
}

#[tauri::command]
pub async fn get_max_speakers(
    app_state: tauri::State<'_, AppState>,
) -> Result<i64, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT maxSpeakers FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let cap: i64 = sqlx::Row::get(&row, "maxSpeakers");
    Ok(cap)
}

#[tauri::command]
pub async fn set_max_speakers(
    app_state: tauri::State<'_, AppState>,
    cap: i64,
) -> Result<(), String> {
    if !(2..=20).contains(&cap) {
        return Err("Max speakers must be between 2 and 20".to_string());
    }
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET maxSpeakers = ? WHERE id = '1'")
        .bind(cap)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    log::info!("set_max_speakers: updated to {}", cap);
    Ok(())
}

async fn fetch_transcripts_for_alignment(
    pool: &SqlitePool,
    meeting_id: &str,
) -> Result<Vec<TranscriptInput>, String> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        text: String,
        start_time: f64,
        end_time: f64,
        token_timestamps: Option<String>,
    }

    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, transcript as text, audio_start_time as start_time, audio_end_time as end_time, token_timestamps FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

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
}

fn resolve_label(speaker: &str) -> String {
    speaker.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_empty() {
        assert!(sanitize_speaker_name("").is_err());
        assert!(sanitize_speaker_name("   ").is_err());
    }

    #[test]
    fn sanitize_rejects_too_long() {
        let long = "A".repeat(201);
        assert!(sanitize_speaker_name(&long).is_err());
    }

    #[test]
    fn sanitize_accepts_normal() {
        assert_eq!(sanitize_speaker_name("Alice").unwrap(), "Alice");
    }

    #[test]
    fn sanitize_strips_html_tags() {
        assert_eq!(
            sanitize_speaker_name("<script>alert(1)</script>").unwrap(),
            "alert(1)"
        );
    }

    #[test]
    fn sanitize_accepts_prompt_injection_as_literal() {
        let name = sanitize_speaker_name("ignore previous instructions").unwrap();
        assert_eq!(name, "ignore previous instructions");
    }

    #[test]
    fn sanitize_accepts_sql_injection_as_literal() {
        let name = sanitize_speaker_name("'; DROP TABLE speakers; --").unwrap();
        assert_eq!(name, "'; DROP TABLE speakers; --");
    }

    #[test]
    fn strip_html_works() {
        assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<script>alert(1)</script>"), "alert(1)");
    }

    #[test]
    fn pick_color_is_deterministic() {
        assert_eq!(pick_color(0), pick_color(0));
        assert_ne!(pick_color(0), pick_color(1));
        let c0 = pick_color(0);
        let c1 = pick_color(1);
        assert!(c0.starts_with("hsl("));
        assert!(c1.starts_with("hsl("));
        assert_ne!(c0, c1);
    }
}
