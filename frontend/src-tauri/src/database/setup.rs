use log::{info, warn};
use tauri::{AppHandle, Emitter, Manager};

use super::manager::DatabaseManager;
use crate::audio::speaker::registry::SpeakerIdentificationPort;
use crate::audio::speaker::sherpa_adapter::SherpaOnnxRegistryAdapter;
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

        app.manage(app_state);
        info!("Database initialized successfully");
    }

    Ok(())
}

async fn hydrate_speaker_registry(
    pool: &sqlx::SqlitePool,
    registry: &std::sync::Mutex<Option<SherpaOnnxRegistryAdapter>>,
) {
    let embeddings = match SpeakerRepository::list_all_embeddings(pool).await {
        Ok(e) => e,
        Err(e) => {
            warn!("Speaker registry hydration failed (query): {}", e);
            return;
        }
    };

    if embeddings.is_empty() {
        info!("No stored speaker embeddings — registry empty");
        return;
    }

    let mut per_speaker: std::collections::HashMap<String, Vec<Vec<f32>>> =
        std::collections::HashMap::new();
    for (name, embedding) in embeddings {
        per_speaker.entry(name).or_default().push(embedding);
    }

    let dim = 256;
    let adapter = match SherpaOnnxRegistryAdapter::new(dim) {
        Ok(a) => a,
        Err(e) => {
            warn!("Speaker registry hydration failed (create adapter): {}", e);
            return;
        }
    };

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

    let speaker_count = per_speaker.len();
    let mut guard = registry.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(adapter);
    info!("Speaker registry hydrated: {} speakers loaded", speaker_count);
}
