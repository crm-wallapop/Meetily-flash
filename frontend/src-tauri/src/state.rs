use crate::audio::speaker::sherpa_adapter::SherpaOnnxRegistryAdapter;
use crate::database::manager::DatabaseManager;
use std::sync::atomic::AtomicU32;
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
}
