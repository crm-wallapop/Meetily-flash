## 1. Remove live transcription path

- [x] 1.1 Write a failing test `pipeline_does_not_initialise_vad`: assert that starting a recording does NOT construct `ContinuousVadProcessor` and emits no `transcript-update` events
- [x] 1.2 Delete the VAD processor initialisation and the Whisper inference path from `pipeline.rs`; delete unused VAD config plumbing
- [x] 1.3 Remove all `transcript-update` event emissions from the recording path
- [x] 1.4 Remove `transcript-update` listeners from `TranscriptContext.tsx`
- [x] 1.5 Run test 1.1 green; remove now-dead `VadProcessor` callers

## 2. Remove audio checkpoint files and recovery

- [x] 2.1 Write a failing test `incremental_saver_writes_no_ckpt_files`: assert that during a recording the only files written to the meeting folder are `audio.mp4` (in progress) and `metadata.json`
- [x] 2.2 Remove `.ckpt` writes from `incremental_saver.rs`; the saver streams directly to MP4 without intermediate checkpoint files
- [x] 2.3 Remove Tauri commands `recover_audio_from_checkpoints`, `has_audio_checkpoints`, `cleanup_checkpoints` from `lib.rs` and their implementations
- [x] 2.4 Remove the calls to these commands from `useTranscriptRecovery.ts`
- [x] 2.5 Update `recording_gc.rs` to delete any orphan `.ckpt` files found alongside an `audio.mp4` (migration cleanup for pre-existing folders); add test for this
- [x] 2.6 Run test 2.1 green

## 3. IndexedDB schema migration to queue persistence

- [x] 3.1 Write a failing test `indexeddb_queue_schema_v2_supports_status_transitions`: assert the new schema allows insert/update of `{ meetingId, status, queuePosition, pauseReason?, startedAt?, completedAt?, lastError? }` and rejects unknown statuses
- [x] 3.2 Define the v2 schema in `indexedDBService.ts` with the new object store; bump the IndexedDB version number to trigger `onupgradeneeded`
- [x] 3.3 Implement a one-time migration in `onupgradeneeded`: read any existing v1 transcript chunks, present them through the legacy recovery path on first launch only, then drop the old object stores. After migration completes, store a `migration_v2_complete` flag
- [x] 3.4 Remove all live-transcript-chunk write paths from the frontend (any code calling `indexedDBService.saveTranscript`/equivalent)
- [x] 3.5 Run test 3.1 green

## 4. Transcription queue use case (Rust)

- [x] 4.1 Write a failing test `queue_enqueues_job_on_stop_recording`: assert that `stop_recording` (normal path) enqueues a job with the new meeting's id and audio path; `cancel_recording` does not
- [x] 4.2 Create `use_cases/transcription_queue.rs` with a `TranscriptionQueue` struct holding `Vec<Job>` behind a `Mutex`, plus a single worker task spawned at app start
- [x] 4.3 Implement `enqueue(meeting_id, audio_path)`, `cancel(meeting_id)`, `pause_all()`, `resume_all()`, `get_state() -> QueueSnapshot`
- [x] 4.4 Add `enqueue_transcription_job(meeting_id, audio_path)` Tauri command; call it from `useRecordingStop.ts` after `saveMeeting()` returns the new meeting's UUID — enqueue is frontend-initiated, not from `recording_manager.rs`
- [x] 4.5 Run test 4.1 green

## 5. Scheduler gates

- [x] 5.1 Write a failing test `scheduler_pauses_when_recording_phase_is_recording`: with `RECORDING_PHASE = Recording`, `scheduler.can_run()` returns false; with `Idle`, true
- [x] 5.2 Write a failing test `scheduler_pauses_when_meeting_detected`: with `meeting_detector` reporting an active call, `can_run()` returns false
- [x] 5.3 Write a failing test `scheduler_pauses_on_sustained_cpu_load`: with mocked CPU samples >70 % for 30 s, `can_run()` returns false; on <70 % for 30 s, returns true (hysteresis)
- [x] 5.4 Write a failing test `scheduler_pauses_on_sustained_ram_load`: same shape as 5.3 with RAM at 80 % threshold
- [x] 5.5 Write a failing test `scheduler_pauses_when_manually_paused`: with `manual_pause_all = true`, `can_run()` returns false regardless of other gates
- [x] 5.6 Implement the `Scheduler` struct in `use_cases/transcription_queue.rs` with the five AND-ed gates; use `sysinfo` for CPU/RAM samples polled at 5 s intervals
- [x] 5.7 Implement hysteresis: gate goes "busy" on N consecutive samples above threshold, returns to "clear" on N consecutive samples below
- [x] 5.8 Run tests 5.1–5.5 green

## 6. Worker loop with pause/resume

- [x] 6.1 Write a failing test `worker_yields_at_chunk_boundary_when_scheduler_says_pause`: queue a job, set `SHOULD_YIELD` mid-run, assert the worker exits between chunks (not mid-chunk) and the job state is `paused`
- [x] 6.2 Write a failing test `worker_resumes_paused_job_when_gates_clear`: with a paused job and all gates clear, assert the worker picks it up and continues from where it stopped
- [x] 6.3 Write a failing test `worker_processes_jobs_in_fifo_order`: enqueue three jobs, assert they run in order
- [x] 6.4 Add `SHOULD_YIELD: AtomicBool` and the chunk-boundary check inside the retranscription loop in `retranscription.rs`; expose a clean exit path that leaves the job resumable
- [x] 6.5 Implement the queue worker as an async task that wakes on enqueue/scheduler-state-change events and processes pending jobs
- [x] 6.6 Run tests 6.1–6.3 green

## 7. Summary chain under the same scheduler

- [x] 7.1 Write a failing test `summary_fires_after_transcription_when_provider_configured`: mock an LLM provider; assert summary runs after transcription completes; assert it does NOT run when no provider is configured
- [x] 7.2 Write a failing test `summary_obeys_scheduler_gates`: with a provider configured and the scheduler set to pause, the summary job stays in `paused` until gates clear
- [x] 7.3 In `retranscription.rs`, on successful completion, transition the job to `phase: 'summarising'` (if provider configured) and re-enter the worker loop so the same `can_run()` check applies before LLM invocation
- [x] 7.4 Emit `transcription-queue-changed` after each phase transition so the UI updates
- [x] 7.5 Run tests 7.1–7.2 green

## 8. Tauri command surface and events

- [x] 8.1 Add Tauri commands: `pause_all_background_work`, `resume_all_background_work`, `get_queue_state`, `cancel_queued_job(meeting_id)`
- [x] 8.2 Emit `transcription-queue-changed` event whenever any job state or scheduler state changes; payload is a full `QueueSnapshot`
- [x] 8.3 Add TypeScript types for the new events and command payloads in `frontend/src/services/`
- [x] 8.4 Run `cargo check` and `pnpm tsc --noEmit` — both green

## 9. Recovery modal repurpose

- [x] 9.1 Write a failing test `recovery_modal_lists_pending_jobs_from_indexeddb`: seed IndexedDB with one `pending` and one `in_progress` job from a "previous session" (timestamps > 15 s ago); assert the modal lists both
- [x] 9.2 Update `useTranscriptRecovery.ts`: `checkForRecoverableTranscripts` reads queue rows with status `pending` or `in_progress` and timestamps older than the 15 s startup-grace window
- [x] 9.3 Update `recoverMeeting`: instead of reassembling audio + saving chunks, re-enqueue the job by calling the queue use case via Tauri
- [x] 9.4 Remove all calls to `recover_audio_from_checkpoints`, `has_audio_checkpoints`, `cleanup_checkpoints` (already removed in §2)
- [x] 9.5 Run test 9.1 green

## 10. Frontend queue UI

- [x] 10.1 Replace the live transcript panel during recording with a static message: "Recording — transcript will be generated after you stop."
- [x] 10.2 Add a per-meeting state badge in the meeting view driven by `transcription-queue-changed`: `Transcribing 34%` / `Queued #N` / `Paused — <reason>` / `Done` / `Failed`
- [x] 10.3 Add a global queue indicator in the app shell showing `N transcriptions queued (status)` with a Pause/Resume button bound to `pause_all_background_work` / `resume_all_background_work`
- [x] 10.4 Add a "Cancel" button on per-meeting queue rows, calling `cancel_queued_job`
- [x] 10.5 Vitest tests for queue UI render logic: `paused-due-to-recording`, `paused-due-to-cpu`, `running`, `queued`, `done`, `failed` states each render the expected label

## 11. Migration safety net

- [x] 11.1 Write a test for the legacy-recovery one-shot path: with v1 IndexedDB schema present at startup, the legacy recovery modal flow runs once, the user can accept or dismiss, then the v2 schema replaces v1 and the flag is set
- [x] 11.2 Implement the one-shot legacy path in `useTranscriptRecovery.ts` gated by absence of `migration_v2_complete`
- [x] 11.3 Run test 11.1 green

## 12. Verification

- [x] 12.1 Run full suite: `cargo test`, `pytest backend/`, `pnpm test`, `pnpm lint` — all green
- [x] 12.2 Manual smoke: start recording, stop normally → confirm MP4 saved, queue picks up the job, transcript appears, summary fires if provider configured
- [~] 12.3 Manual smoke: start recording, cancel → confirm no MP4 on disk, no job enqueued, no transcription runs *(deferred — no UI cancel button exists; cancel path covered by unit tests in `recording_commands.rs`)*
- [ ] 12.4 Manual smoke: back-to-back recordings → confirm M2 starts immediately after M1's stop, M1's transcription job pauses while M2 is recording, resumes when M2 stops, both eventually complete in order
- [ ] 12.5 Manual smoke: long meeting → confirm progress percentage advances, cancel button works, Pause All button stops all work, Resume restarts cleanly
- [ ] 12.6 Manual smoke: kill app mid-transcription → relaunch → confirm recovery modal lists the in-flight meeting and accepting it re-runs the job

## 13. Spec drift and archive prep

- [x] 13.1 Re-read this proposal, design, and the two delta specs; confirm implementation matches every scenario; amend any deltas that drifted during apply
- [x] 13.2 Confirm `transcription-scheduler-advanced` change exists and its proposal references the hardcoded defaults this change ships, ready for follow-up

## 14. Smoke-test 12.5 follow-ups

Found by manual smoke test 12.5: Pause All was unresponsive, Resume button never surfaced, and the per-meeting badge stayed on `Transcribing…` with no progress percentage.

- [x] 14.1 Write failing tests: `pause_all_asserts_should_yield_for_in_flight_chunk`, `resume_all_clears_should_yield`, `pause_all_sets_manual_pause_all_flag`, `snapshot_exposes_manual_pause_all_flag` (red against the existing implementation)
- [x] 14.2 Move `manual_pause_all` ownership into `TranscriptionQueue::pause_all` / `resume_all`; assert `SHOULD_YIELD` on pause and clear on resume so in-flight retranscription yields at its next chunk boundary (spec `Manual pause overrides everything`)
- [x] 14.3 Add `manual_pause_all: bool` to `QueueSnapshot`; populate it in `get_state()` and in both worker-loop snapshot construction sites
- [x] 14.4 Make `pause_all_background_work` / `resume_all_background_work` emit `transcription-queue-changed` after the state change (so the UI doesn't wait for the next worker-loop transition)
- [x] 14.5 Update TypeScript `QueueSnapshot` to include `manual_pause_all`; update `GlobalQueueIndicator` to drive Pause/Resume from that flag rather than the fragile `allPaused` per-job heuristic — Resume must surface immediately after click, even while an in-flight job is still finishing its chunk
- [x] 14.6 Subscribe to existing `retranscription-progress` events in `useQueueJobStatus` and decorate the matching `QueueJob` with `progress_percent`; `queueJobLabel` renders `Transcribing N%` when known, `Transcribing…` otherwise
- [x] 14.7 Add `sonner` toast on successful `cancelQueuedJob` so the badge disappearance has user-visible confirmation (the spec keeps `cancel = remove`; the toast addresses only the UX gap)
- [x] 14.8 Update vitest `queue-ui-render-logic.test.ts` so the embedded `deriveIndicatorState` mirrors the new `manual_pause_all`-driven logic; add a regression case where `manual_pause_all = true` AND a job is still `InProgress` (the pre-fix bug) and `Transcribing N%` label coverage
- [x] 14.9 Amend `specs/post-meeting-pipeline/spec.md`: scope `progress %` and `manual_pause_all` into this change; capture the synchronous-emit scenario for manual pause/resume
- [x] 14.10 Code review pass 1 — fixed worker pickup-window race: `worker_loop` now folds the pickup, InProgress flip, AND manual_pause_all re-check into a single critical section, so a concurrent `pause_all` between `can_run()` and the InProgress flip flips the job to Paused without dispatching (regression test `worker_marks_pending_job_paused_under_manual_pause_without_dispatching`)
- [x] 14.11 Code review pass 1 — added `serial_test` dev-dep; annotated every test that touches the global `SHOULD_YIELD` static (or `spawn_worker` which writes it) with `#[serial]` so default-parallel `cargo test` runs are deterministic
- [ ] 14.12 Smoke test 12.5 again after Rust rebuild: Pause All flips toggle to Resume immediately, in-flight job ends up Paused (not Done), Resume picks it up, progress % advances during transcription, Cancel shows toast
