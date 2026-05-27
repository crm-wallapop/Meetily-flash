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
    meeting_id: String,
) -> Result<u64, String> {
    let cleared = SpeakerRepository::clear_auto_speaker_labels(pool.inner(), &meeting_id)
        .await
        .map_err(|e| e.to_string())?;

    log::info!(
        "rediarize_meeting: cleared {} auto labels for meeting {}",
        cleared,
        meeting_id
    );

    // Re-enqueue would happen via the queue system — caller should enqueue after this
    Ok(cleared)
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
