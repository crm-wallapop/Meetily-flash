## 1. Rust LLM Client

- [x] 1.1 Raise `REQUEST_TIMEOUT_DURATION` from `Duration::from_secs(300)` to `Duration::from_secs(900)` in `frontend/src-tauri/src/summary/llm_client.rs` line 8
- [x] 1.2 Fix error message at line 272: change `"LLM request timed out after 60 seconds"` to `"LLM request timed out after 15 minutes"`
- [x] 1.3 Fix error message at line 285: change `"LLM request timed out after 60 seconds"` to `"LLM request timed out after 15 minutes"`

## 2. Verification

- [x] 2.1 Run `cargo check` in `frontend/src-tauri/` — confirms the constant and strings compile cleanly
- [x] 2.2 Run `cargo test` in `frontend/src-tauri/` — confirms no regressions
