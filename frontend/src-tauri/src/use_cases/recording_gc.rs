use std::path::Path;

use crate::database::manager::DatabaseManager;
use crate::database::models::MeetingModel;

#[derive(Debug, Default)]
pub struct GcReport {
    pub orphan_rows_deleted: usize,
    pub orphan_files_deleted: usize,
    pub errors: Vec<String>,
}

/// Removes orphan DB rows (whose folder no longer exists on disk) and orphan audio
/// folders (not referenced by any meeting row). Called at startup before the detector
/// is spawned. Errors are logged and collected — startup never fails due to GC.
pub async fn run_startup_gc(db: &DatabaseManager, recordings_dir: &Path) -> GcReport {
    let mut report = GcReport::default();

    let pool = db.pool();

    // ── 1. Orphan DB rows ──────────────────────────────────────────────────
    // A meeting row is "orphan" if its folder_path is set but the folder is gone.

    let meetings: Vec<MeetingModel> = match sqlx::query_as::<_, MeetingModel>(
        "SELECT id, title, created_at, updated_at, folder_path FROM meetings",
    )
    .fetch_all(pool)
    .await
    {
        Ok(m) => m,
        Err(e) => {
            report
                .errors
                .push(format!("gc: failed to list meetings: {e}"));
            return report;
        }
    };

    // Collect valid folder paths for the orphan-file check below.
    // Path strings must be stored and compared via PathBuf::to_string_lossy() consistently —
    // both DB writes (recording_saver.rs) and disk reads below use native separators, so
    // the HashSet comparison is safe. If any code path stores paths with forward slashes,
    // valid folders could be mistakenly treated as orphans.
    let mut known_folders: std::collections::HashSet<String> = std::collections::HashSet::new();

    for meeting in &meetings {
        if let Some(ref folder) = meeting.folder_path {
            if !folder.is_empty() {
                if std::path::Path::new(folder).exists() {
                    known_folders.insert(folder.clone());
                } else {
                    // Folder missing → delete the row.
                    match sqlx::query("DELETE FROM meetings WHERE id = ?")
                        .bind(&meeting.id)
                        .execute(pool)
                        .await
                    {
                        Ok(_) => {
                            log::info!(
                                "gc: deleted orphan row for meeting {} (missing folder {})",
                                meeting.id,
                                folder
                            );
                            report.orphan_rows_deleted += 1;
                        }
                        Err(e) => {
                            report.errors.push(format!(
                                "gc: failed to delete orphan row {}: {e}",
                                meeting.id
                            ));
                        }
                    }
                }
            }
        }
    }

    // ── 2. Orphan audio folders ────────────────────────────────────────────
    // A folder in the recordings dir is "orphan" if no meeting row has it as folder_path.

    if recordings_dir.exists() {
        let entries = match std::fs::read_dir(recordings_dir) {
            Ok(e) => e,
            Err(e) => {
                report
                    .errors
                    .push(format!("gc: failed to read recordings dir: {e}"));
                return report;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let path_str = path.to_string_lossy().to_string();
            if known_folders.contains(&path_str) {
                continue; // Referenced by a meeting row — keep it.
            }

            // Orphan folder — remove it recursively.
            match std::fs::remove_dir_all(&path) {
                Ok(_) => {
                    log::info!("gc: deleted orphan folder {}", path_str);
                    report.orphan_files_deleted += 1;
                }
                Err(e) => {
                    report.errors.push(format!(
                        "gc: failed to delete orphan folder {path_str}: {e}"
                    ));
                }
            }
        }
    }

    // ── 3. Orphan .checkpoints directories (pre-migration cleanup) ────────────
    // Folders from before the post-meeting-transcription change may have a
    // .checkpoints/ subdirectory alongside audio.mp4. Now that the saver writes
    // directly to audio.mp4, these subdirectories are dead and should be removed.

    let all_candidate_dirs: Vec<std::path::PathBuf> = {
        let mut v: Vec<_> = known_folders
            .iter()
            .map(|s| std::path::PathBuf::from(s))
            .collect();

        // Also scan the recordings dir for any folder not yet in known_folders
        // (e.g., meetings whose audio.mp4 exists but has no DB row — already cleaned
        // above, but we handle the case defensively).
        if recordings_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(recordings_dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        v.push(p);
                    }
                }
            }
        }
        v.sort();
        v.dedup();
        v
    };

    for folder in &all_candidate_dirs {
        let ckpt_dir = folder.join(".checkpoints");
        if ckpt_dir.is_dir() {
            match std::fs::remove_dir_all(&ckpt_dir) {
                Ok(_) => {
                    log::info!("gc: removed orphan .checkpoints dir in {}", folder.display());
                    report.orphan_files_deleted += 1;
                }
                Err(e) => {
                    report.errors.push(format!(
                        "gc: failed to remove .checkpoints in {}: {e}",
                        folder.display()
                    ));
                }
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Helper: in-memory SQLite database with the meetings schema.
    async fn test_db() -> (DatabaseManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.sqlite").to_string_lossy().to_string();
        let db = DatabaseManager::new(&db_path, &db_path).await.unwrap();
        (db, dir)
    }

    // Task 6.2: orphan DB row (missing folder) → GC deletes the row.
    #[tokio::test]
    async fn gc_deletes_orphan_db_row() {
        let (db, _dir) = test_db().await;
        let pool = db.pool();

        let missing_folder = "/nonexistent/path/to/meeting";
        let now = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("meeting-gc-test-1")
        .bind("Orphan meeting")
        .bind(now)
        .bind(now)
        .bind(missing_folder)
        .execute(pool)
        .await
        .unwrap();

        let recordings_dir = std::path::PathBuf::from("/nonexistent/recordings");
        let report = run_startup_gc(&db, &recordings_dir).await;

        assert_eq!(report.orphan_rows_deleted, 1, "should delete one orphan row");
        assert!(report.errors.is_empty(), "no errors expected: {:?}", report.errors);
    }

    // Task 6.3: orphan folder (no meeting row references it) → GC deletes the folder.
    #[tokio::test]
    async fn gc_deletes_orphan_folder() {
        let (db, _dir) = test_db().await;

        let temp_recordings = TempDir::new().unwrap();
        // Create an orphan subfolder in the recordings dir.
        let orphan_folder = temp_recordings.path().join("orphan_meeting_2024-01-01_10-00");
        std::fs::create_dir_all(&orphan_folder).unwrap();
        // Create a dummy audio file inside.
        std::fs::write(orphan_folder.join("audio.wav"), b"RIFF").unwrap();

        let report = run_startup_gc(&db, temp_recordings.path()).await;

        assert_eq!(report.orphan_files_deleted, 1, "should delete one orphan folder");
        assert!(!orphan_folder.exists(), "orphan folder should be gone");
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);
    }

    // Task 6.4: valid meeting + valid folder → GC touches neither.
    #[tokio::test]
    async fn gc_leaves_valid_meetings_untouched() {
        let (db, _dir) = test_db().await;
        let pool = db.pool();

        let temp_recordings = TempDir::new().unwrap();
        let valid_folder = temp_recordings.path().join("valid_meeting_2024-01-01_10-00");
        std::fs::create_dir_all(&valid_folder).unwrap();

        let folder_str = valid_folder.to_string_lossy().to_string();
        let now = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("meeting-gc-valid")
        .bind("Valid meeting")
        .bind(now)
        .bind(now)
        .bind(&folder_str)
        .execute(pool)
        .await
        .unwrap();

        let report = run_startup_gc(&db, temp_recordings.path()).await;

        assert_eq!(report.orphan_rows_deleted, 0);
        assert_eq!(report.orphan_files_deleted, 0);
        assert!(valid_folder.exists(), "valid folder must remain");
    }

    // Task 6.5 (renamed): GC deletes all orphan folders when both succeed.
    #[tokio::test]
    async fn gc_deletes_multiple_orphan_folders() {
        let (db, _dir) = test_db().await;

        let temp_recordings = TempDir::new().unwrap();
        let orphan1 = temp_recordings.path().join("orphan1");
        std::fs::create_dir_all(&orphan1).unwrap();
        let orphan2 = temp_recordings.path().join("orphan2");
        std::fs::create_dir_all(&orphan2).unwrap();

        let report = run_startup_gc(&db, temp_recordings.path()).await;

        assert_eq!(report.orphan_files_deleted, 2);
        assert!(report.errors.is_empty());
    }

    // Task 2.5: orphan .checkpoints dir alongside audio.mp4 → GC deletes it.
    #[tokio::test]
    async fn gc_deletes_orphan_checkpoints_dir() {
        let (db, _dir) = test_db().await;
        let pool = db.pool();

        let temp_recordings = TempDir::new().unwrap();
        let meeting_folder = temp_recordings.path().join("Meeting_With_Ckpt_2024-01-01_10-00");
        std::fs::create_dir_all(&meeting_folder).unwrap();

        // Simulate a pre-migration folder: has audio.mp4 AND an orphan .checkpoints dir
        std::fs::write(meeting_folder.join("audio.mp4"), b"fake-mp4").unwrap();
        let ckpt_dir = meeting_folder.join(".checkpoints");
        std::fs::create_dir_all(&ckpt_dir).unwrap();
        std::fs::write(ckpt_dir.join("audio_chunk_000.mp4"), b"chunk0").unwrap();

        // Register the meeting in DB so the folder is considered "known"
        let folder_str = meeting_folder.to_string_lossy().to_string();
        let now = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("meeting-ckpt-cleanup")
        .bind("Meeting with checkpoint dir")
        .bind(now)
        .bind(now)
        .bind(&folder_str)
        .execute(pool)
        .await
        .unwrap();

        let report = run_startup_gc(&db, temp_recordings.path()).await;

        assert!(!ckpt_dir.exists(), ".checkpoints dir must be removed by GC");
        assert!(report.errors.is_empty(), "no errors expected: {:?}", report.errors);
        assert!(report.orphan_files_deleted >= 1, "should count checkpoint dir as deleted");
    }

    // Task 6.5: partial failure — one folder locked, GC records error and continues
    // to clean the other. File locking on Windows is enforced via open file handle;
    // on non-Windows the folder permissions are made read-only to prevent removal.
    #[tokio::test]
    async fn gc_records_error_and_continues_on_partial_failure() {
        let (db, _dir) = test_db().await;

        let temp_recordings = TempDir::new().unwrap();

        // orphan1: will be locked so GC cannot delete it
        let orphan1 = temp_recordings.path().join("orphan1");
        std::fs::create_dir_all(&orphan1).unwrap();
        let locked_file_path = orphan1.join("audio.wav");
        std::fs::write(&locked_file_path, b"RIFF").unwrap();

        // orphan2: clean deletion expected
        let orphan2 = temp_recordings.path().join("orphan2");
        std::fs::create_dir_all(&orphan2).unwrap();

        // Hold an exclusive write handle on Windows, or make the folder read-only elsewhere.
        #[cfg(target_os = "windows")]
        let _lock = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .share_mode(0) // no sharing — prevents deletion
                .open(&locked_file_path)
        };

        #[cfg(not(target_os = "windows"))]
        {
            let mut perms = std::fs::metadata(&orphan1).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            { perms.set_readonly(true); }
            std::fs::set_permissions(&orphan1, perms).unwrap();
        }

        let report = run_startup_gc(&db, temp_recordings.path()).await;

        // orphan2 deleted; orphan1 failed with an error recorded
        assert_eq!(report.orphan_files_deleted, 1, "one folder deleted successfully");
        assert!(!report.errors.is_empty(), "error recorded for locked folder");

        // cleanup: restore permissions so TempDir drop succeeds
        #[cfg(not(target_os = "windows"))]
        {
            let mut perms = std::fs::metadata(&orphan1).unwrap().permissions();
            perms.set_readonly(false);
            std::fs::set_permissions(&orphan1, perms).unwrap();
        }
    }
}
