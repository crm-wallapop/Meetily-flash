## Why

When a user deletes a meeting from the UI, `delete_meeting_with_transaction` (meeting.rs:233) removes the DB rows (transcripts, transcript_chunks, summary_processes, meetings) but never touches the filesystem. The frontend calls `api_delete_meeting` which delegates to the repository — neither path reads `folder_path` to remove the meeting directory. Over time, stale empty folders (and sometimes folders with leftover `metadata.json` or other non-audio artifacts) accumulate in the recordings directory.

## What Changes

- `delete_meeting_with_transaction` (or its caller `api_delete_meeting`) SHALL read `folder_path` from the meetings row before deleting it, then call `std::fs::remove_dir_all` on the folder after the transaction commits.
- If `folder_path` is `None` or the folder doesn't exist on disk, the deletion succeeds silently (no error).
- If `remove_dir_all` fails (permission denied, file in use), the DB deletion still succeeds — the error is logged but not surfaced to the user. Orphaned folders are preferable to blocking the delete.

## Capabilities

### Modified Capabilities
- `recording-lifecycle`: meeting deletion now cleans up the on-disk folder in addition to the DB rows.

## Impact

**Rust (`frontend/src-tauri/`)**
- `database/repositories/meeting.rs`: `delete_meeting` reads `folder_path` before the transaction, then removes the directory after commit.
- `api/api.rs`: `api_delete_meeting` — no change needed if the repository handles it.

**No schema migration, no frontend changes.**
