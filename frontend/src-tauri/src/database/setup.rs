use log::{info, warn};
use tauri::{AppHandle, Emitter, Manager};

use super::manager::DatabaseManager;
use crate::audio::speaker::registry::SpeakerIdentificationPort;
use crate::audio::speaker::sherpa_adapter::CosineRegistryAdapter;
use crate::audio::speaker::types::EmbeddingVector;
use crate::database::repositories::speaker::SpeakerRepository;
use crate::state::AppState;

/// Initialize database on app startup
/// Handles first launch detection and conditional initialization
pub async fn initialize_database_on_startup(app: &AppHandle) -> Result<(), String> {
    let is_first_launch = DatabaseManager::is_first_launch(app)
        .await
        .map_err(|e| format!("Failed to check first launch status: {}", e))?;

    if is_first_launch {
        info!("First launch detected - will notify window when ready");

        let app_handle = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            app_handle
                .emit("first-launch-detected", ())
                .expect("Failed to emit first-launch-detected event");
            info!("Emitted first-launch-detected after delay");
        });
    } else {
        let db_manager = DatabaseManager::new_from_app_handle(app)
            .await
            .map_err(|e| format!("Failed to initialize database manager: {}", e))?;

        let app_state = AppState::new(db_manager);
        let pool = app_state.db_manager.pool().clone();

        hydrate_speaker_registry(&pool, &app_state.speaker_registry).await;
        app_state.sync_threshold_from_db().await;

        app.manage(app_state);
        info!("Database initialized successfully");
    }

    Ok(())
}

async fn hydrate_speaker_registry(
    pool: &sqlx::SqlitePool,
    registry: &std::sync::Mutex<Option<CosineRegistryAdapter>>,
) {
    let embeddings = match SpeakerRepository::list_all_embeddings(pool).await {
        Ok(e) => e,
        Err(e) => {
            warn!("Speaker registry hydration failed (query): {}", e);
            return;
        }
    };

    match build_hydrated_registry(embeddings) {
        Some((adapter, speaker_count)) => {
            let mut guard = registry.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(adapter);
            info!("Speaker registry hydrated: {} speakers loaded", speaker_count);
        }
        None => info!("No stored speaker embeddings — registry empty"),
    }
}

/// Pure core of hydration, split out so the dim-192 regression (task 4.4) is
/// testable without a database fixture. Returns None when there is nothing to
/// load.
fn build_hydrated_registry(
    embeddings: Vec<(String, Vec<f32>)>,
) -> Option<(CosineRegistryAdapter, usize)> {
    if embeddings.is_empty() {
        return None;
    }

    let mut per_speaker: std::collections::HashMap<String, Vec<Vec<f32>>> =
        std::collections::HashMap::new();
    for (name, embedding) in embeddings {
        per_speaker.entry(name).or_default().push(embedding);
    }

    // nemo_titanet embeddings are 192-dim. (This was previously hardcoded to
    // 256 — a pre-existing bug: `EmbeddingVector::from_slice(v, 256)` rejected
    // every stored 192-dim vector, so hydration silently loaded ZERO speakers
    // and cross-meeting matching was dead. Fixed by the Part B port; task 4.4
    // asserts hydration loads N>0 at dim 192.)
    let dim = crate::audio::speaker::nemo_extractor::NEMO_EMBEDDING_DIM;
    let adapter = CosineRegistryAdapter::new(dim).ok()?;

    for (name, vecs) in &per_speaker {
        let emb_vectors: Vec<EmbeddingVector> = vecs
            .iter()
            .filter_map(|v| EmbeddingVector::from_slice(v, dim).ok())
            .collect();
        if emb_vectors.is_empty() {
            continue;
        }
        if let Err(e) = adapter.add_list(name, &emb_vectors) {
            warn!("Failed to add {} embeddings for '{}': {}", emb_vectors.len(), name, e);
        }
    }

    Some((adapter, per_speaker.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 4.4 regression: stored 192-dim vectors must ALL hydrate. The old
    /// hardcoded `dim = 256` rejected every stored vector, silently loading
    /// zero speakers and killing cross-meeting matching.
    #[test]
    fn registry_hydration_loads_speakers_at_192_dim() {
        let dim = crate::audio::speaker::nemo_extractor::NEMO_EMBEDDING_DIM;
        assert_eq!(dim, 192, "nemo_titanet output dim");
        // Distinct DIRECTIONS (constant-filled vectors would be mutually
        // parallel under cosine and tie). Alice spans e0, Bob spans e1.
        let mk_alice = |e0: f32| {
            let mut v = vec![0.0f32; dim];
            v[0] = e0;
            v[1] = (1.0 - e0) * 0.5;
            v
        };
        let mut bob = vec![0.0f32; dim];
        bob[1] = 1.0;
        let embeddings = vec![
            ("Alice".to_string(), mk_alice(1.0)),
            ("Alice".to_string(), mk_alice(0.95)),
            ("Bob".to_string(), bob),
        ];
        let (adapter, count) = build_hydrated_registry(embeddings).expect("non-empty input loads");
        assert_eq!(count, 2, "two distinct speakers");
        assert_eq!(adapter.list_speakers().unwrap().len(), 2);

        // A query along Alice's direction matches her through the hydrated
        // registry (per-vector scan over the loaded vectors).
        let query = crate::audio::speaker::types::EmbeddingVector::from_slice(&mk_alice(1.0), dim).unwrap();
        assert_eq!(adapter.search(&query, 0.5).unwrap().as_deref(), Some("Alice"));
    }

    #[test]
    fn registry_hydration_empty_input_is_none() {
        assert!(build_hydrated_registry(Vec::new()).is_none());
    }
}
