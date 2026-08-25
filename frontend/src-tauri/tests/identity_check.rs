//! Identity check for the cde5c264 anchors (user request 2026-08-25):
//! the boundary oracles proved the ~46:58 interjection survives as a
//! DISTINCT cluster and the ~17:37 join exists — but never checked WHO the
//! clusters are. This scores fresh TitaNet embeddings for each anchor window
//! against every ENROLLED speaker vector in the real DB and prints the full
//! cosine table, so identity can be judged by margin, not assertion.
//!
//! Run:
//!   cargo test --release --test identity_check -- --ignored --nocapture

const MEETING_ID: &str = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";
const DB_PATH: &str = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";

/// (label, start_secs, end_secs) — windows chosen to be single-speaker-ish:
/// the two complaint anchors plus surrounding-run contrast windows.
const WINDOWS: &[(&str, f64, f64)] = &[
    ("interjection ~46:58 (the complaint)", 2818.0, 2836.0),
    ("run BEFORE interjection (~47:00 host)", 2795.0, 2812.0),
    ("join ~17:37 (third voice enters)", 1057.0, 1072.0),
    ("mid-meeting main run (~20:00)", 1200.0, 1215.0),
];

#[tokio::test]
#[ignore] // requires on-disk recording + models + local DB
async fn anchor_windows_vs_enrolled_speakers() {
    use std::collections::BTreeMap;

    // 1. Enrolled vectors, SCOPED TO THIS MEETING: speaker_embeddings rows
    // here have speaker_id=NULL, so pooling by cluster_label merges
    // same-labeled clusters across unrelated meetings. Only rows whose
    // source_meeting_id is the anchor meeting are identity evidence for it.
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", DB_PATH))
        .await
        .expect("connect DB");
    let rows = sqlx::query(
        "SELECT e.embedding, COALESCE(s.name, e.cluster_label) as name \
         FROM speaker_embeddings e LEFT JOIN speakers s ON e.speaker_id = s.id \
         WHERE e.source_meeting_id = ?",
    )
    .bind(MEETING_ID)
    .fetch_all(&pool)
    .await
    .expect("fetch enrolled");
    let mut enrolled: BTreeMap<String, Vec<Vec<f32>>> = BTreeMap::new();
    for r in rows {
        let blob: Vec<u8> = sqlx::Row::get(&r, "embedding");
        let name: String = sqlx::Row::get(&r, "name");
        let vec: Vec<f32> = blob
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(
            vec.len(),
            app_lib::audio::speaker::nemo_extractor::NEMO_EMBEDDING_DIM,
            "enrolled embedding for '{}' is {}-dim",
            name,
            vec.len()
        );
        enrolled.entry(name).or_default().push(vec);
    }
    assert!(!enrolled.is_empty(), "no enrolled speakers in DB");
    eprintln!("ENROLLED:");
    for (name, vecs) in &enrolled {
        eprintln!("  {:<12} {} vector(s)", name, vecs.len());
    }

    // 2. Resolve + decode the meeting audio through the production decoder.
    let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(MEETING_ID)
        .fetch_one(&pool)
        .await
        .expect("fetch meeting");
    let folder = sqlx::Row::get::<Option<String>, _>(&row, "folder_path").expect("folder_path");
    drop(pool);
    let dir = std::path::PathBuf::from(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio file in {}", folder));
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode");
    let samples = decoded.to_whisper_format(); // 16 kHz mono, whole file
    eprintln!(
        "AUDIO: {:.1}s @16k from {}",
        samples.len() as f64 / 16000.0,
        audio_path.display()
    );

    // 3. Extractor at the shipped model.
    let model = dirs::home_dir()
        .expect("home")
        .join(".meetily-models")
        .join(app_lib::audio::speaker::model_download::embedding_filename());
    assert!(model.exists(), "nemo model missing at {}", model.display());
    let extractor =
        app_lib::audio::speaker::nemo_extractor::NemoEmbeddingExtractor::new(
            model.to_str().unwrap(),
        )
        .expect("build extractor");

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na <= f32::EPSILON || nb <= f32::EPSILON {
            0.0
        } else {
            dot / (na * nb)
        }
    }

    // 4. Score each window against every enrolled speaker (max over that
    // speaker's stored vectors — matches CosineRegistryAdapter search).
    for (label, ws, we) in WINDOWS {
        let s = ((*ws * 16000.0) as usize).min(samples.len());
        let e = ((*we * 16000.0) as usize).min(samples.len());
        let emb = match extractor.extract_embedding(&samples[s..e], 16000) {
            Some(v) => v,
            None => {
                eprintln!("\n{} [{}-{}]: extractor DROPPED the window", label, ws, we);
                continue;
            }
        };
        let mut scored: Vec<(String, f32)> = enrolled
            .iter()
            .map(|(name, vecs)| {
                (
                    name.clone(),
                    vecs.iter().map(|v| cosine(&emb, v)).fold(f32::MIN, f32::max),
                )
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        eprintln!("\n{} [{}-{}] ({}s):", label, ws, we, we - ws);
        for (name, cos) in &scored {
            eprintln!("  {:+.4}  {}", cos, name);
        }
        if scored.len() >= 2 {
            eprintln!(
                "  margin best-vs-2nd: {:+.4}",
                scored[0].1 - scored[1].1
            );
        }
    }
}
