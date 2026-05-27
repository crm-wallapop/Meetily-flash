use crate::audio::speaker::alignment::TranscriptInput;
use crate::audio::speaker::diarization::DiarizationPort;
use crate::database::repositories::speaker::SpeakerRepository;
use crate::state::AppState;
use sqlx::SqlitePool;
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
    // Strip HTML tags to prevent XSS
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
    // Golden angle spacing — maximizes hue separation between consecutive speakers.
    let hue = (index as f64 * 137.508) % 360.0;
    format!("hsl({}, 65%, 55%)", hue.round() as u16)
}

#[tauri::command]
pub async fn label_speaker(
    pool: tauri::State<'_, SqlitePool>,
    meeting_id: String,
    cluster_label: String,
    speaker_name: String,
) -> Result<String, String> {
    let name = sanitize_speaker_name(&speaker_name)?;

    // Verify meeting exists
    let meeting_exists = sqlx::query("SELECT id FROM meetings WHERE id = ?")
        .bind(&meeting_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    if meeting_exists.is_none() {
        return Err(format!("Meeting not found: {}", meeting_id));
    }

    // Verify cluster has transcripts
    let cluster_rows = sqlx::query(
        "SELECT COUNT(*) as count FROM transcripts WHERE meeting_id = ? AND speaker_label = ?",
    )
    .bind(&meeting_id)
    .bind(&cluster_label)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    let count: i64 = sqlx::Row::get(&cluster_rows, "count");
    if count == 0 {
        return Err(format!(
            "No transcripts found for cluster '{}' in meeting {}",
            cluster_label, meeting_id
        ));
    }

    // Create or find speaker
    let speaker_id = format!("speaker-{}", Uuid::new_v4());

    #[derive(sqlx::FromRow)]
    struct SpeakerIdColor {
        id: String,
        color: String,
    }

    // Check if a speaker with this name already exists
    let existing = sqlx::query_as::<_, SpeakerIdColor>(
        "SELECT id, color FROM speakers WHERE name = ?",
    )
    .bind(&name)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    let (final_speaker_id, final_color) = match existing {
        Some(row) => (row.id, row.color),
        None => {
            let speaker_count = SpeakerRepository::list_speakers(pool.inner())
                .await
                .map(|s| s.len())
                .unwrap_or(0);
            let color = pick_color(speaker_count);
            SpeakerRepository::create_speaker(pool.inner(), &speaker_id, &name, &color)
                .await
                .map_err(|e| e.to_string())?;
            (speaker_id, color)
        }
    };

    // Update all transcript rows for this cluster
    let updated = SpeakerRepository::update_meeting_speakers(
        pool.inner(),
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
    pool: tauri::State<'_, SqlitePool>,
) -> Result<serde_json::Value, String> {
    let speakers = SpeakerRepository::list_speakers(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(speakers).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_speaker_cmd(
    pool: tauri::State<'_, SqlitePool>,
    speaker_id: String,
) -> Result<bool, String> {
    SpeakerRepository::remove_speaker(pool.inner(), &speaker_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn rediarize_meeting(
    pool: tauri::State<'_, SqlitePool>,
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<u64, String> {
    // Clear existing auto labels
    let cleared = SpeakerRepository::clear_auto_speaker_labels(pool.inner(), &meeting_id)
        .await
        .map_err(|e| e.to_string())?;

    log::info!(
        "rediarize_meeting: cleared {} auto labels for meeting {}",
        cleared,
        meeting_id
    );

    // Look up meeting folder path
    let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(&meeting_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    let folder_path: Option<String> = row.and_then(|r| sqlx::Row::get(&r, "folder_path"));
    let Some(folder) = folder_path else {
        log::warn!("rediarize_meeting: no folder_path for meeting {}", meeting_id);
        return Ok(cleared);
    };

    // Find audio file
    let folder_path = std::path::Path::new(&folder);
    let audio_path = folder_path.join("audio.mp4");
    if !audio_path.exists() {
        log::warn!(
            "rediarize_meeting: no audio.mp4 in {}",
            folder_path.display()
        );
        return Ok(cleared);
    }

    // Resolve model paths
    let models_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".meetily-models");
    let embedding_path = models_dir.join("3dspeaker-embedding.onnx");
    let segmentation_path = models_dir.join("pyannote-segmentation.onnx");

    if !embedding_path.exists() || !segmentation_path.exists() {
        return Err(
            "Speaker models not found. Download pyannote-segmentation.onnx and 3dspeaker-embedding.onnx to ~/.meetily-models/".to_string()
        );
    }

    // Decode audio
    let decoded = crate::audio::decoder::decode_audio_file(&audio_path)
        .map_err(|e| format!("Audio decode failed: {}", e))?;
    let mono = to_mono_f32(&decoded);

    // Create adapter with shared threshold
    let threshold_fp = app_state.speaker_merge_threshold_fp.load(Ordering::Relaxed);
    let shared_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(threshold_fp));
    let adapter = super::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
        embedding_path.to_str().unwrap_or(""),
        segmentation_path.to_str().unwrap_or(""),
        shared_fp,
    )
    .map_err(|e| format!("Failed to create diarization adapter: {}", e))?;

    // Run diarization
    let segments = adapter.process(&mono, 16000)
        .map_err(|e| format!("Diarization failed: {}", e))?;

    if segments.is_empty() {
        log::info!("rediarize_meeting: 0 speakers detected for meeting {}", meeting_id);
        return Ok(cleared);
    }

    let num_speakers: std::collections::HashSet<u32> =
        segments.iter().map(|s| s.speaker_id).collect();
    log::info!(
        "rediarize_meeting: detected {} speakers for meeting {}",
        num_speakers.len(),
        meeting_id
    );

    // Fetch transcripts and align
    let transcripts = fetch_transcripts_for_alignment(pool.inner(), &meeting_id).await
        .map_err(|e| format!("Failed to fetch transcripts: {}", e))?;

    if transcripts.is_empty() {
        return Ok(cleared);
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
        SpeakerRepository::update_transcript_speaker(pool.inner(), &seg.original_id, &label, "auto")
            .await
            .map_err(|e| e.to_string())?;
        segments_labeled += 1;
    }

    log::info!(
        "rediarize_meeting: labeled {} segments for meeting {}",
        segments_labeled,
        meeting_id
    );

    Ok(segments_labeled)
}

#[tauri::command]
pub async fn get_speaker_merge_threshold(
    pool: tauri::State<'_, SqlitePool>,
) -> Result<f64, String> {
    let row = sqlx::query("SELECT speaker_merge_threshold FROM settings LIMIT 1")
        .fetch_one(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    let threshold: f64 = sqlx::Row::get(&row, "speaker_merge_threshold");
    Ok(threshold)
}

#[tauri::command]
pub async fn set_speaker_merge_threshold(
    pool: tauri::State<'_, SqlitePool>,
    app_state: tauri::State<'_, AppState>,
    threshold: f64,
) -> Result<(), String> {
    if !(0.30..=0.70).contains(&threshold) {
        return Err("Threshold must be between 0.30 and 0.70".to_string());
    }
    sqlx::query("UPDATE settings SET speaker_merge_threshold = ? WHERE id = '1'")
        .bind(threshold)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    let fp = (threshold as f32 * 65536.0) as u32;
    app_state.speaker_merge_threshold_fp.store(fp, Ordering::Relaxed);
    log::info!("set_speaker_merge_threshold: updated to {}", threshold);
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
        // Golden angle ensures consecutive indices are far apart in hue
        let c0 = pick_color(0);
        let c1 = pick_color(1);
        assert!(c0.starts_with("hsl("));
        assert!(c1.starts_with("hsl("));
        assert_ne!(c0, c1);
    }
}
