use crate::database::repositories::speaker::SpeakerRepository;
use sqlx::SqlitePool;
use uuid::Uuid;

const COLOR_PALETTE: &[&str] = &[
    "#4A90D9", "#7B68EE", "#20B2AA", "#FF6B6B", "#FFA500",
    "#9370DB", "#3CB371", "#FF69B4", "#6495ED", "#F08080",
    "#8FBC8F", "#DDA0DD", "#87CEEB", "#F4A460", "#BA55D3",
    "#66CDAA", "#FF7F50", "#6A5ACD", "#48D1CC", "#DB7093",
];

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
    COLOR_PALETTE[index % COLOR_PALETTE.len()].to_string()
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
    let color = pick_color(0); // Will be refined with existing speaker lookup

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
        assert_eq!(pick_color(0), "#4A90D9");
        assert_eq!(pick_color(0), pick_color(20)); // wraps around
        assert_eq!(pick_color(1), "#7B68EE");
    }
}
