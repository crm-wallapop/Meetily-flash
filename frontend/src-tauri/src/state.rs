use crate::database::manager::DatabaseManager;
use std::sync::atomic::AtomicU32;

pub struct AppState {
    pub db_manager: DatabaseManager,
    /// Speaker merge threshold as Q16 fixed-point (value / 65536.0 = threshold).
    pub speaker_merge_threshold_fp: AtomicU32,
}

impl AppState {
    pub fn new(db_manager: DatabaseManager) -> Self {
        Self {
            db_manager,
            speaker_merge_threshold_fp: AtomicU32::new((0.50f32 * 65536.0) as u32),
        }
    }
}
