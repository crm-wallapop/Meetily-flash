use crate::audio::speaker::sherpa_adapter::SherpaOnnxRegistryAdapter;
use crate::database::manager::DatabaseManager;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub struct AppState {
    pub db_manager: DatabaseManager,
    /// Speaker merge threshold as Q16 fixed-point (value / 65536.0 = threshold).
    pub speaker_merge_threshold_fp: AtomicU32,
    /// In-memory speaker registry for cross-meeting matching.
    /// Hydrated from speaker_embeddings table on startup.
    pub speaker_registry: Arc<Mutex<Option<SherpaOnnxRegistryAdapter>>>,
}

impl AppState {
    pub fn new(db_manager: DatabaseManager) -> Self {
        Self {
            db_manager,
            speaker_merge_threshold_fp: AtomicU32::new((0.40f32 * 65536.0) as u32),
            speaker_registry: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn sync_threshold_from_db(&self) {
        let row = sqlx::query("SELECT speakerMergeThreshold FROM settings LIMIT 1")
            .fetch_optional(self.db_manager.pool())
            .await;
        match row {
            Ok(Some(r)) => match sqlx::Row::try_get::<f64, _>(&r, "speakerMergeThreshold") {
                Ok(threshold) => {
                    let fp = (threshold as f32 * 65536.0) as u32;
                    self.speaker_merge_threshold_fp.store(fp, Ordering::Relaxed);
                    log::info!("synced speaker merge threshold from DB: {:.2}", threshold);
                }
                Err(e) => log::warn!("failed to read speakerMergeThreshold: {}", e),
            },
            Ok(None) => log::info!("no settings row, using default threshold 0.40"),
            Err(e) => log::warn!("failed to query speakerMergeThreshold: {}", e),
        }
    }
}
