## Context

Today the audio pipeline runs two parallel paths during recording: (1) recording path — raw PCM is mixed and encoded to MP4; (2) transcription path — VAD processes 50 ms mixed chunks and forwards speech chunks to Whisper in real time, emitting `transcript-update` events and persisting chunks to IndexedDB. The retranscription path (`retranscription.rs`) already exists as a user-triggered "re-process" action that decodes the saved MP4, runs VAD with a 2 s redemption window over the full file, and sends contiguous chunks to Whisper with full-file context. Empirical observation on 2026-05-13 confirmed that live transcription produces unusable output (mixed-Unicode hallucinations from VAD noise mis-fires), while retranscription on the same MP4 produces coherent text.

`fix-stop-responsiveness` (now in archive prep) introduced `RECORDING_PHASE: AtomicU8` with values `Idle | Recording | Saving`. With live transcription gone, `Saving` becomes near-instantaneous (close MP4, write metadata, enqueue job) — the 2 m 15 s lag this change was created to fix disappears at its root. The phase atomic remains useful as a scheduling signal for the transcription queue.

This change makes retranscription the **primary** path triggered automatically after every recording, removes live transcription entirely, and adds a scheduler so background transcription/summary jobs never compete for GPU/CPU/RAM with the user's active work.

## Goals / Non-Goals

**Goals:**
- Every recording is transcribed post-stop from the saved MP4. No user action required.
- If an LLM provider is configured, summarisation is chained immediately after transcription. Same gating policy as transcription.
- The UI reflects queue state per meeting (`Transcribing 34%` / `Queued #2` / `Paused — reason`) and globally (`N queued (paused)`).
- A scheduler pauses transcription and summary jobs when the system is busy: actively recording, in a Meet/Zoom call, or under sustained CPU/RAM load. Paused jobs finish their current Whisper chunk before yielding.
- A global "Pause all background work" override is always available.
- Queue state persists across crashes via IndexedDB so unfinished jobs are detected and offered on next launch via the existing recovery modal.
- Live transcription code is removed cleanly — no dormant code paths.

**Non-Goals:**
- Real-time transcript display during recording (intentionally removed; the live panel is unused and the live output is unusable).
- User-configurable scheduler thresholds (deferred to `transcription-scheduler-advanced`).
- GPU-load gate in the scheduler (deferred — start with CPU/RAM, measure, then decide).
- Changes to the MP4 encoding format, RNNoise behaviour, or VAD tuning (covered by `tune-vad-rnnoise`).
- Changes to summarisation prompts or LLM client internals.
- Multi-language detection or speaker diarisation changes.

## Decisions

### D1: Reuse `retranscription.rs` as the primary path, not a new module

`start_retranscription` already handles decode → VAD → Whisper → write `transcripts.json`, with progress events and a cancellation flag. The only additions needed are: (a) an auto-mode entry point callable from the new queue use case, and (b) a hook to trigger summarisation on completion. Creating a parallel "primary transcription" module would duplicate logic.

Alternatives considered:
- New dedicated `post_recording_pipeline.rs`: cleaner separation but 80 % duplication of `retranscription.rs`. Rejected — YAGNI.
- Merge the summary trigger into `retranscription.rs` directly: simpler call graph but mixes concerns. Accepted as a pragmatic trade-off given the small scope; the trigger is a single conditional call at the end of the function.

### D2: Summary chain is opt-in via existing LLM configuration, not a new toggle

If the user has no LLM provider configured, the chain stops after transcription. If one is configured, summarisation fires automatically. No new UI toggle — the existing provider configuration is the gate.

### D3: Live transcription is removed entirely, with no opt-back toggle

The original proposal kept live transcription behind a `transcription_mode: "live"` setting for power-users. That toggle is removed. The user confirmed in exploration that the live transcript panel is never used; the live output we sampled was hallucinated garbage. Keeping the live code paths gated behind a setting would leave dormant code that is structurally unable to produce usable output. Delete the path, save the maintenance.

If a future reason emerges to want live transcription, it would be a different design (longer chunks, different VAD windows) — not the same code path resurrected.

### D4: Queue + scheduler live in a new use case, `transcription_queue.rs`

The queue and scheduler are application logic that orchestrates adapters (audio file → Whisper → optional LLM). Per CLAUDE.md §2a hexagonal rules, that belongs in `use_cases/`, not in an audio adapter or a Tauri command body.

Sketch of the use case:

```
┌─────────────────────────────────────────────────────────────┐
│ TranscriptionQueue                                          │
│                                                             │
│  enqueue(meeting_id, audio_path)                            │
│  cancel(meeting_id)                                         │
│  pause_all() / resume_all()                                 │
│  get_state() -> QueueSnapshot                               │
│                                                             │
│  ── worker loop ─────────────────────────────────────       │
│   loop:                                                     │
│     job = next_pending_job()                                │
│     if !scheduler.can_run() { wait; continue }              │
│     run_chunk(job)            // calls into retranscription │
│     if scheduler.should_yield() { mark_paused; continue }   │
│     if job.complete { fire summary if provider; mark done } │
│                                                             │
│  ── scheduler ──────────────────────────────────────────    │
│   can_run() ⇔                                               │
│     RECORDING_PHASE == Idle AND                             │
│     !meeting_detector.in_call() AND                         │
│     !sysload.cpu_over(70%, 30s) AND                         │
│     !sysload.ram_over(80%, 30s) AND                         │
│     !manual_pause_all                                       │
└─────────────────────────────────────────────────────────────┘
```

The queue is single-worker (one job at a time) — Whisper is a singleton resource and parallel jobs would only contend on the GPU. Multiple recordings stack as `pending` rows; the worker processes them sequentially.

### D5: Pause granularity is chunk-boundary; jobs are never interrupted mid-chunk

When the scheduler signals "yield" mid-job, the worker finishes the current Whisper chunk before checking gates again. With typical 30 s chunks via `retranscription.rs`, pause latency is bounded at ~30 s. Acceptable trade-off: smaller chunks would hurt Whisper accuracy (less context) and increase model-load amortisation overhead, defeating the proposal's primary motivation.

Implementation: existing `RETRANSCRIPTION_CANCELLED: AtomicBool` is repurposed for the cancel path (unchanged). A new `SHOULD_YIELD: AtomicBool` is read at chunk boundaries inside the retranscription loop; on `true` the worker exits cleanly, the job stays in `paused` state, and the queue worker re-enters its outer loop.

### D6: IndexedDB is repurposed as the queue persistence layer

Today `indexedDBService.ts` stores live-transcript chunks per meeting. With live transcription removed, those writes disappear. Rather than delete the entire store, repurpose it: schema becomes the queue state itself (`meetingId`, `status`, `queuePosition`, `pauseReason`, timestamps, `lastError`). Reasons:

- IndexedDB persists across crashes and restarts — exactly what queue state needs.
- The recovery modal's existing scaffolding (`useTranscriptRecovery.ts`, `checkForRecoverableTranscripts`) can be repointed at the new schema with minimal change.
- One persistence layer for one purpose is cleaner than deleting one and adding another.

Alternatives considered:
- Persist queue state in SQLite (backend DB): adds a backend round-trip per state change, the backend may not be running for local-only users. Rejected.
- Persist in a Rust-side JSON file: works but reinvents the IndexedDB infrastructure we already have wired into the recovery flow. Rejected.

### D7: Recovery modal switches its backing scan

The existing modal flow (detect → present list → user accepts → recover) is preserved. What changes:

- **Detection**: instead of scanning IndexedDB for transcript-chunk rows and verifying audio checkpoints via `has_audio_checkpoints`, scan IndexedDB for queue rows whose status is `pending` or `in_progress` from a previous app session.
- **Recovery action**: instead of reassembling audio from checkpoint files and saving live transcript chunks to backend DB, simply re-enqueue the job. The MP4 is already on disk; the queue worker picks it up.
- **Removed code**: `recover_audio_from_checkpoints`, `has_audio_checkpoints`, `cleanup_checkpoints`, the `.ckpt` file writes in `incremental_saver.rs`.

The modal becomes both simpler and more honest — there's nothing to "recover" mechanically, just a job to resume.

### D8: Scheduler gates are AND-ed and hardcoded in this change

`can_run()` is `gate1 ∧ gate2 ∧ gate3 ∧ gate4 ∧ gate5`. Any single busy signal pauses all background work. This is the conservative default — false-positive pauses are recoverable (resume button), false-negative greediness is the failure mode the user explicitly flagged ("silent performance killer").

Hardcoded values for this change: CPU 70 % over 30 s; RAM 80 % over 30 s. These are conservative starting points based on typical headroom for an active Meet call + recording (~30–40 % CPU baseline observed during the 2026-05-13 captures). Configurable thresholds and the `aggressive | polite | manual` mode preset land in `transcription-scheduler-advanced` once we have measured behaviour.

Cross-platform CPU/RAM reading: the `sysinfo` crate already in the workspace (used by `audio/diagnostics.rs`) covers Windows/macOS/Linux. No new dependency.

### D9: Same scheduler governs transcription and summary jobs

Both are heavy background work; both should yield to the user. Implementation: the summary trigger after transcription completion also enqueues a job (or extends the existing job with a `phase: 'transcribing' | 'summarising'` field) so the worker loop applies the same `can_run()` check before invoking the LLM.

This keeps the policy story coherent — one set of rules, one set of pause UI affordances. If we wanted summary to run more aggressively (since it's a single LLM call, not chunks), that would be a Q3 follow-up; not justified now.

### D10: Auto-triggered jobs and cancellation interact via the queue, not directly

`cancel_recording` already prevents `stop_recording` from finalising the MP4 (it deletes the folder instead). Since the queue is fed only from the normal stop path, cancelled recordings naturally never enqueue. We do not need a separate "if cancelled don't enqueue" guard — the data flow does it.

For the `cancel_queued_job` command (user removes a meeting from the queue): set status to `cancelled`, the worker loop skips it. If the worker is currently running the cancelled job, the existing `RETRANSCRIPTION_CANCELLED` flag aborts it at the next chunk boundary.

## Risks / Trade-offs

- **[Risk]** Users accustomed to seeing live transcript text are surprised. → Mitigation: the live panel is replaced by an explicit message; the user confirmed in exploration that the live panel is unused. First-run notice not needed; the static message during recording covers it.

- **[Risk]** Transcription latency on long meetings is noticeable. At ~3× real-time on Vulkan, a 1-hour meeting takes ~20 minutes; on CPU it could be 40–60 minutes. → Mitigation: queue UI shows progress; user can cancel; user can re-trigger from the meeting view if they cancel and want it later. Latency is hardware-bound and not a defect of this change.

- **[Risk]** MP4 decode adds quality loss via AAC round-trip. → 192 kbps AAC for speech is perceptually transparent; quality gap is below Whisper's noise floor. Not a practical concern.

- **[Risk]** Hardcoded thresholds (CPU 70 %, RAM 80 %) don't fit all hardware. A user on a constrained laptop may find transcription effectively never runs. → Mitigation: `transcription-scheduler-advanced` exposes configurable thresholds. In the interim the global "Pause all background work" button has an inverse — when paused, the worker is fully gated; when resumed and gates clear, the user can confirm jobs are flowing.

- **[Risk]** Scheduler oscillation: CPU rises just over the threshold for sustained 30 s, scheduler pauses; CPU drops below, scheduler resumes; CPU rises again. → Mitigation: hysteresis on the sustained-duration check (resume requires 30 s *below* threshold), implemented as a single state machine on the gate.

- **[Risk]** A long-running job with the global pause toggled on never completes. → Acceptable; user invoked the pause deliberately. Resume restarts the worker; the in-flight chunk completed before the pause check, no work is lost.

- **[Risk]** Queue persistence in IndexedDB is browser-scoped; if the user clears site data the queue is wiped. → Acceptable; the MP4 files on disk are the source of truth. Worst case is the user has to manually re-trigger transcription per meeting. The recovery modal scan can be augmented to fall back to filesystem scan if IndexedDB is empty (future-proofing, not in this change's scope).

- **[Trade-off]** Removing `recover_audio_from_checkpoints` deletes a meaningful amount of working code. Acceptable: the code's purpose was reassembling audio from checkpoints in the live-recording era, which no longer exists.

- **[Trade-off]** Auto-triggered batch transcription means the user can't "skip" transcription for a meeting they don't care about. Acceptable: the per-meeting cancel in the queue UI gives equivalent control; the default of "always transcribe" matches the user's stated mental model.

## Migration Plan

- IndexedDB schema migration: on first launch after upgrade, detect the old schema (transcript chunks), present them in the recovery modal one last time using the legacy flow, then drop the old object stores and create the new queue schema. After migration runs once, the legacy code path is unreachable and is removed in the next change.
- Filesystem migration: existing `.ckpt` files from prior recordings are deleted by the GC pass on next launch (orphan-file logic already handles unknown files in meeting folders; tighten the rule so any folder whose `audio.mp4` is also present has its `.ckpt` files cleaned).
- No DB schema change required (meeting rows are unchanged).

## Open Questions

- Should the recovery modal differentiate "this meeting never transcribed" from "this meeting transcribed but summary failed"? Probably yes for clarity; mark with sub-status in the queue row.
- Display formatting for "Paused — reason": fixed strings vs i18n-ready? Default to fixed strings, mark for i18n later.
- Should `cancel_queued_job` delete the meeting from the DB or just remove it from the queue? Recommend: remove from queue only; meeting metadata stays so the user can re-trigger transcription manually from the meeting view.
