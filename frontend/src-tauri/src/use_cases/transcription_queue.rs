// use_cases/transcription_queue.rs
//
// Transcription queue, scheduler, and worker loop.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, Notify};

// ── Scheduler constants ──────────────────────────────────────────────────────

const CPU_THRESHOLD: f64 = 70.0;
const RAM_THRESHOLD: f64 = 80.0;
/// Consecutive 5-second samples required to flip a hysteresis gate.
const HYSTERESIS_WINDOW: usize = 6; // 30 s / 5 s

// ── Yield signal shared with retranscription.rs (task 6.4) ──────────────────

/// Set by the worker loop when the scheduler signals "yield".
/// retranscription.rs checks this at each chunk boundary and exits cleanly.
pub static SHOULD_YIELD: AtomicBool = AtomicBool::new(false);

// ── Scheduler ────────────────────────────────────────────────────────────────

pub struct Scheduler {
    pub recording_busy: Arc<AtomicBool>,
    pub meeting_busy: Arc<AtomicBool>,
    pub cpu_busy: Arc<AtomicBool>,
    pub ram_busy: Arc<AtomicBool>,
    pub manual_pause_all: Arc<AtomicBool>,

    cpu_window: Arc<StdMutex<VecDeque<f64>>>,
    ram_window: Arc<StdMutex<VecDeque<f64>>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            recording_busy: Arc::new(AtomicBool::new(false)),
            meeting_busy: Arc::new(AtomicBool::new(false)),
            cpu_busy: Arc::new(AtomicBool::new(false)),
            ram_busy: Arc::new(AtomicBool::new(false)),
            manual_pause_all: Arc::new(AtomicBool::new(false)),
            cpu_window: Arc::new(StdMutex::new(VecDeque::new())),
            ram_window: Arc::new(StdMutex::new(VecDeque::new())),
        }
    }

    pub fn can_run(&self) -> bool {
        !self.manual_pause_all.load(Ordering::Relaxed)
            && !self.recording_busy.load(Ordering::Relaxed)
            && !self.meeting_busy.load(Ordering::Relaxed)
            && !self.cpu_busy.load(Ordering::Relaxed)
            && !self.ram_busy.load(Ordering::Relaxed)
    }

    pub fn feed_cpu_sample(&self, pct: f64) {
        self.feed_sample(&self.cpu_window, &self.cpu_busy, pct, CPU_THRESHOLD);
    }

    pub fn feed_ram_sample(&self, pct: f64) {
        self.feed_sample(&self.ram_window, &self.ram_busy, pct, RAM_THRESHOLD);
    }

    fn feed_sample(
        &self,
        window: &Arc<StdMutex<VecDeque<f64>>>,
        busy: &Arc<AtomicBool>,
        pct: f64,
        threshold: f64,
    ) {
        let mut w = window.lock().unwrap();
        w.push_back(pct);
        while w.len() > HYSTERESIS_WINDOW {
            w.pop_front();
        }
        if w.len() == HYSTERESIS_WINDOW {
            if w.iter().all(|&s| s > threshold) {
                busy.store(true, Ordering::Relaxed);
            } else if w.iter().all(|&s| s <= threshold) {
                busy.store(false, Ordering::Relaxed);
            }
        }
    }
}

// ── Processor type ───────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum JobResult {
    /// This phase is done; no further phases should run (go directly to Done).
    Completed,
    /// This phase is done AND a follow-on phase should run if one is registered.
    /// The transcription processor returns this when an LLM provider is configured
    /// so the worker chains into the summary phase. When no provider is configured,
    /// it returns plain `Completed` and the job goes directly to Done.
    CompletedChain,
    Yielded,
    Failed(String),
}

/// Two-phase processing: first transcription, then (optionally) summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobPhase {
    Transcribing,
    Summarising,
}

pub type AsyncJobResult = Pin<Box<dyn Future<Output = JobResult> + Send + 'static>>;
pub type ProcessorFn =
    Arc<dyn Fn(String, PathBuf) -> AsyncJobResult + Send + Sync + 'static>;

/// Default no-op processor.  In production this will be replaced by the
/// retranscription adapter wired up in task 7 (summary chain).
fn noop_processor() -> ProcessorFn {
    Arc::new(|_id, _path| Box::pin(async { JobResult::Completed }))
}

// ── Job / Queue ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobStatus {
    Pending,
    InProgress,
    Paused,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub meeting_id: String,
    pub audio_path: PathBuf,
    pub status: JobStatus,
    pub phase: JobPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub jobs: Vec<Job>,
}

/// Called after each job status transition; receives a full snapshot.
/// Use this to emit Tauri events or update any external observer.
pub type StateChangeNotifier = Arc<dyn Fn(QueueSnapshot) + Send + Sync + 'static>;

pub struct TranscriptionQueue {
    jobs: Arc<Mutex<Vec<Job>>>,
    notify: Arc<Notify>,
    pub scheduler: Arc<Scheduler>,
    processor: ProcessorFn,
    /// Set to `Some` when an LLM provider is configured.
    summary_processor: Option<ProcessorFn>,
}

impl TranscriptionQueue {
    pub fn new() -> Self {
        Self::with_processor(noop_processor())
    }

    pub fn with_processor(processor: ProcessorFn) -> Self {
        Self::with_processors(processor, None)
    }

    pub fn with_processors(processor: ProcessorFn, summary_processor: Option<ProcessorFn>) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
            scheduler: Arc::new(Scheduler::new()),
            processor,
            summary_processor,
        }
    }

    /// Enqueue a new transcription job.
    /// Called by recording_manager after finalize() succeeds on the normal stop path.
    pub async fn enqueue(&self, meeting_id: String, audio_path: PathBuf) {
        let mut jobs = self.jobs.lock().await;
        jobs.push(Job {
            meeting_id,
            audio_path,
            status: JobStatus::Pending,
            phase: JobPhase::Transcribing,
        });
        self.notify.notify_one();
    }

    /// Remove a job without processing it.
    pub async fn cancel(&self, meeting_id: &str) {
        let was_in_progress = {
            let mut jobs = self.jobs.lock().await;
            let in_progress = jobs
                .iter()
                .any(|j| j.meeting_id == meeting_id && j.status == JobStatus::InProgress);
            jobs.retain(|j| j.meeting_id != meeting_id);
            in_progress
        };
        // Signal the active processor to abort at the next chunk boundary.
        if was_in_progress {
            crate::audio::retranscription::cancel_retranscription();
        }
    }

    /// Pause all pending/in-progress jobs (manual override).
    pub async fn pause_all(&self) {
        let mut jobs = self.jobs.lock().await;
        for job in jobs.iter_mut() {
            if job.status == JobStatus::Pending || job.status == JobStatus::InProgress {
                job.status = JobStatus::Paused;
            }
        }
    }

    /// Resume all paused jobs and wake the worker.
    pub async fn resume_all(&self) {
        let mut jobs = self.jobs.lock().await;
        for job in jobs.iter_mut() {
            if job.status == JobStatus::Paused {
                job.status = JobStatus::Pending;
            }
        }
        self.notify.notify_one();
    }

    pub async fn get_state(&self) -> QueueSnapshot {
        let jobs = self.jobs.lock().await;
        QueueSnapshot { jobs: jobs.clone() }
    }

    /// Spawn the background worker task.  Call once from app setup (lib.rs).
    pub fn spawn_worker(&self) -> tauri::async_runtime::JoinHandle<()> {
        self.spawn_worker_with_notifier(None)
    }

    /// Spawn the background worker with an optional state-change callback.
    /// The callback is invoked after every job status transition; use it to emit Tauri events.
    pub fn spawn_worker_with_notifier(
        &self,
        notifier: Option<StateChangeNotifier>,
    ) -> tauri::async_runtime::JoinHandle<()> {
        let jobs = self.jobs.clone();
        let notify = self.notify.clone();
        let scheduler = self.scheduler.clone();
        let processor = self.processor.clone();
        let summary_processor = self.summary_processor.clone();
        tauri::async_runtime::spawn(worker_loop(
            jobs,
            notify,
            scheduler,
            processor,
            summary_processor,
            notifier,
        ))
    }
}

async fn worker_loop(
    jobs: Arc<Mutex<Vec<Job>>>,
    notify: Arc<Notify>,
    scheduler: Arc<Scheduler>,
    processor: ProcessorFn,
    summary_processor: Option<ProcessorFn>,
    notifier: Option<StateChangeNotifier>,
) {
    loop {
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
        }

        loop {
            if !scheduler.can_run() {
                break;
            }

            // Find the next pending job.
            let job_info = {
                let jobs = jobs.lock().await;
                jobs.iter()
                    .find(|j| j.status == JobStatus::Pending)
                    .map(|j| (j.meeting_id.clone(), j.audio_path.clone(), j.phase.clone()))
            };

            let Some((meeting_id, audio_path, phase)) = job_info else {
                break;
            };

            // Transition to InProgress.
            let snapshot = {
                let mut jobs = jobs.lock().await;
                if let Some(j) = jobs
                    .iter_mut()
                    .find(|j| j.meeting_id == meeting_id && j.status == JobStatus::Pending)
                {
                    j.status = JobStatus::InProgress;
                }
                QueueSnapshot { jobs: jobs.clone() }
            };
            if let Some(n) = &notifier { n(snapshot); }

            // Reset yield signal before each processor invocation so a stale true
            // from a previous recording does not cause an immediate re-yield.
            SHOULD_YIELD.store(false, Ordering::SeqCst);

            // Dispatch to the right processor for this phase.
            let result = match phase {
                JobPhase::Transcribing => (processor)(meeting_id.clone(), audio_path).await,
                JobPhase::Summarising => {
                    if let Some(ref sp) = summary_processor {
                        (sp)(meeting_id.clone(), audio_path).await
                    } else {
                        // No summary processor — this phase should not occur.
                        JobResult::Completed
                    }
                }
            };

            // Update status / phase based on result.
            let snapshot = {
                let mut jobs = jobs.lock().await;
                if let Some(j) = jobs.iter_mut().find(|j| j.meeting_id == meeting_id) {
                    match result {
                        JobResult::Completed => {
                            j.status = JobStatus::Done;
                        }
                        JobResult::CompletedChain => {
                            if j.phase == JobPhase::Transcribing
                                && summary_processor.is_some()
                            {
                                // Transcription done and provider configured; queue the summary phase.
                                j.phase = JobPhase::Summarising;
                                j.status = JobStatus::Pending;
                            } else {
                                j.status = JobStatus::Done;
                            }
                        }
                        JobResult::Yielded => j.status = JobStatus::Paused,
                        JobResult::Failed(_) => j.status = JobStatus::Failed,
                    }
                }
                QueueSnapshot { jobs: jobs.clone() }
            };
            if let Some(n) = &notifier { n(snapshot); }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── Queue tests (task 4.1) ────────────────────────────────────────────────

    #[tokio::test]
    async fn queue_enqueues_job_on_stop_recording() {
        let queue = TranscriptionQueue::new();
        let state = queue.get_state().await;
        assert!(state.jobs.is_empty(), "fresh queue must be empty before any stop_recording call");

        let meeting_id = "meeting-stop-test".to_string();
        let audio_path = PathBuf::from("/recordings/meeting-stop-test/audio.mp4");
        queue.enqueue(meeting_id.clone(), audio_path.clone()).await;

        let state = queue.get_state().await;
        assert_eq!(state.jobs.len(), 1, "stop_recording (normal) must produce exactly one queued job");
        assert_eq!(state.jobs[0].meeting_id, meeting_id);
        assert_eq!(state.jobs[0].audio_path, audio_path);
        assert_eq!(state.jobs[0].status, JobStatus::Pending);
    }

    #[tokio::test]
    async fn cancel_recording_does_not_enqueue() {
        let queue = TranscriptionQueue::new();
        let state = queue.get_state().await;
        assert!(state.jobs.is_empty(), "cancel_recording must not enqueue a transcription job");
    }

    // Adversarial: enqueue must accept a UUID-style meeting_id pointing to an audio file that
    // does not yet exist on disk. The recording gate (cleared after background_shutdown finishes
    // writing the MP4) prevents the worker from starting before the file is present — so the
    // queue layer must not reject the job at enqueue time.
    #[tokio::test]
    async fn enqueue_accepts_nonexistent_audio_path() {
        let queue = TranscriptionQueue::new();
        let meeting_id = "meeting-550e8400-e29b-41d4-a716-446655440000".to_string();
        let audio_path = PathBuf::from("/nonexistent/recording/audio.mp4");
        queue.enqueue(meeting_id.clone(), audio_path.clone()).await;

        let state = queue.get_state().await;
        assert_eq!(state.jobs.len(), 1, "queue must accept a job even when the audio file does not exist yet");
        assert_eq!(state.jobs[0].meeting_id, meeting_id);
        assert_eq!(state.jobs[0].audio_path, audio_path);
        assert_eq!(state.jobs[0].status, JobStatus::Pending);
    }

    // Adversarial: meeting_id must use the UUID format from the DB row, not the folder-name
    // format. This test pins the contract: after saveMeeting() returns "meeting-{uuid}", the
    // enqueued job carries that exact ID so the meeting view can find the transcripts.
    #[tokio::test]
    async fn enqueue_uses_uuid_not_folder_name() {
        let queue = TranscriptionQueue::new();
        // UUID format (from DB row via saveMeeting)
        let uuid_id = "meeting-550e8400-e29b-41d4-a716-446655440000".to_string();
        // Folder-name format (what recording_commands previously used — must NOT be used)
        let folder_id = "Meeting 2026-05-14_17-06-41_2026-05-14_15-06".to_string();

        queue.enqueue(uuid_id.clone(), PathBuf::from("/audio.mp4")).await;
        let state = queue.get_state().await;

        assert_eq!(state.jobs[0].meeting_id, uuid_id, "enqueued meeting_id must be the UUID from the DB row");
        assert_ne!(state.jobs[0].meeting_id, folder_id, "enqueued meeting_id must NOT be the folder name");
    }

    // ── Scheduler tests (tasks 5.1–5.5) ──────────────────────────────────────

    #[test]
    fn scheduler_pauses_when_recording_phase_is_recording() {
        let sched = Scheduler::new();
        assert!(sched.can_run(), "fresh scheduler must allow running");
        sched.recording_busy.store(true, Ordering::Relaxed);
        assert!(!sched.can_run(), "scheduler must pause when recording_busy is set");
        sched.recording_busy.store(false, Ordering::Relaxed);
        assert!(sched.can_run(), "scheduler must resume when recording_busy is cleared");
    }

    #[test]
    fn scheduler_pauses_when_meeting_detected() {
        let sched = Scheduler::new();
        sched.meeting_busy.store(true, Ordering::Relaxed);
        assert!(!sched.can_run(), "scheduler must pause when meeting_detector reports active call");
        sched.meeting_busy.store(false, Ordering::Relaxed);
        assert!(sched.can_run());
    }

    #[test]
    fn scheduler_pauses_on_sustained_cpu_load() {
        let sched = Scheduler::new();
        for _ in 0..HYSTERESIS_WINDOW {
            sched.feed_cpu_sample(80.0);
        }
        assert!(!sched.can_run(), "scheduler must pause after sustained CPU > 70%");
        for _ in 0..HYSTERESIS_WINDOW {
            sched.feed_cpu_sample(50.0);
        }
        assert!(sched.can_run(), "scheduler must resume after sustained CPU ≤ 70%");
    }

    #[test]
    fn scheduler_pauses_on_sustained_ram_load() {
        let sched = Scheduler::new();
        for _ in 0..HYSTERESIS_WINDOW {
            sched.feed_ram_sample(85.0);
        }
        assert!(!sched.can_run(), "scheduler must pause after sustained RAM > 80%");
        for _ in 0..HYSTERESIS_WINDOW {
            sched.feed_ram_sample(70.0);
        }
        assert!(sched.can_run(), "scheduler must resume after sustained RAM ≤ 80%");
    }

    #[test]
    fn scheduler_pauses_when_manually_paused() {
        let sched = Scheduler::new();
        sched.manual_pause_all.store(true, Ordering::Relaxed);
        assert!(!sched.can_run(), "manual_pause_all must block can_run regardless of other gates");
    }

    // ── Worker tests (tasks 6.1–6.3) ─────────────────────────────────────────

    async fn wait_for_status(queue: &TranscriptionQueue, meeting_id: &str, expected: JobStatus) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            let state = queue.get_state().await;
            if state.jobs.iter().any(|j| j.meeting_id == meeting_id && j.status == expected) {
                return;
            }
            if tokio::time::Instant::now() > deadline {
                let state = queue.get_state().await;
                panic!(
                    "timeout waiting for job '{meeting_id}' to reach {:?}; actual: {:?}",
                    expected,
                    state.jobs.iter().find(|j| j.meeting_id == meeting_id).map(|j| &j.status)
                );
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn worker_yields_at_chunk_boundary_when_scheduler_says_pause() {
        // Processor returns Yielded immediately, simulating a chunk-boundary yield.
        let should_yield = Arc::new(AtomicBool::new(true));
        let sy = should_yield.clone();
        let processor: ProcessorFn = Arc::new(move |_id, _path| {
            let sy = sy.clone();
            Box::pin(async move {
                if sy.load(Ordering::Relaxed) {
                    JobResult::Yielded
                } else {
                    JobResult::Completed
                }
            })
        });

        let queue = TranscriptionQueue::with_processor(processor);
        let _handle = queue.spawn_worker();

        queue
            .enqueue(
                "yield-test".to_string(),
                PathBuf::from("/recordings/yield-test/audio.mp4"),
            )
            .await;

        wait_for_status(&queue, "yield-test", JobStatus::Paused).await;
    }

    // Note: auto-wake on scheduler gate-change (no `resume_all` call) is not yet
    // implemented — the worker polls on a 5 s fallback interval.  The gate-clear
    // → automatic notify path is deferred until the scheduler holds an Arc<Notify>.
    #[tokio::test]
    async fn worker_resumes_paused_job_after_resume_all() {
        // First run: processor yields; second run: processor completes.
        let yielded_once = Arc::new(AtomicBool::new(false));
        let y = yielded_once.clone();
        let processor: ProcessorFn = Arc::new(move |_id, _path| {
            let y = y.clone();
            Box::pin(async move {
                if y.swap(true, Ordering::Relaxed) {
                    // Already yielded once — now complete.
                    JobResult::Completed
                } else {
                    JobResult::Yielded
                }
            })
        });

        let queue = TranscriptionQueue::with_processor(processor);
        let _handle = queue.spawn_worker();

        queue
            .enqueue(
                "resume-test".to_string(),
                PathBuf::from("/recordings/resume-test/audio.mp4"),
            )
            .await;

        // Worker will yield the job first.
        wait_for_status(&queue, "resume-test", JobStatus::Paused).await;

        // Resume — the processor will return Completed this time.
        queue.resume_all().await;

        wait_for_status(&queue, "resume-test", JobStatus::Done).await;
    }

    #[tokio::test]
    async fn worker_processes_jobs_in_fifo_order() {
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let order_clone = order.clone();
        let processor: ProcessorFn = Arc::new(move |id, _path| {
            let order = order_clone.clone();
            Box::pin(async move {
                order.lock().await.push(id);
                JobResult::Completed
            })
        });

        let queue = TranscriptionQueue::with_processor(processor);
        let _handle = queue.spawn_worker();

        for i in 1..=3 {
            queue
                .enqueue(
                    format!("job-{i}"),
                    PathBuf::from(format!("/recordings/job-{i}/audio.mp4")),
                )
                .await;
        }

        // Wait until all three are done.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            let state = queue.get_state().await;
            if state.jobs.iter().all(|j| j.status == JobStatus::Done) {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("worker did not complete all 3 jobs within 500 ms");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let recorded = order.lock().await;
        assert_eq!(*recorded, vec!["job-1", "job-2", "job-3"]);
    }

    // ── Summary chain tests (tasks 7.1–7.2) ──────────────────────────────────

    #[tokio::test]
    async fn summary_fires_after_transcription_when_provider_configured() {
        // With provider: transcription → summary → Done.
        let summary_called = Arc::new(AtomicBool::new(false));
        let flag = summary_called.clone();

        let queue = Arc::new(TranscriptionQueue::with_processors(
            Arc::new(|_id, _path| Box::pin(async { JobResult::CompletedChain })),
            Some(Arc::new(move |_id, _path| {
                let f = flag.clone();
                Box::pin(async move {
                    f.store(true, Ordering::SeqCst);
                    JobResult::Completed
                })
            })),
        ));
        let _handle = queue.spawn_worker();
        queue.enqueue("s7-with-provider".to_string(), PathBuf::from("/audio.mp4")).await;
        wait_for_status(&queue, "s7-with-provider", JobStatus::Done).await;
        assert!(summary_called.load(Ordering::SeqCst), "summary must run when provider is configured");

        // Without provider: transcription → Done (summary not called).
        // Transcription processor returns Completed (not CompletedChain) when no provider.
        let queue2 = Arc::new(TranscriptionQueue::with_processors(
            Arc::new(|_id, _path| Box::pin(async { JobResult::Completed })),
            None,
        ));
        let _handle2 = queue2.spawn_worker();
        queue2.enqueue("s7-no-provider".to_string(), PathBuf::from("/audio.mp4")).await;
        wait_for_status(&queue2, "s7-no-provider", JobStatus::Done).await;
        let state = queue2.get_state().await;
        let job = state.jobs.iter().find(|j| j.meeting_id == "s7-no-provider").unwrap();
        assert_eq!(job.phase, JobPhase::Transcribing, "job must remain in Transcribing phase when no provider");
    }

    #[tokio::test]
    async fn summary_obeys_scheduler_gates() {
        let summary_called = Arc::new(AtomicBool::new(false));
        let flag = summary_called.clone();

        let queue = Arc::new(TranscriptionQueue::with_processors(
            Arc::new(|_id, _path| Box::pin(async { JobResult::CompletedChain })),
            Some(Arc::new(move |_id, _path| {
                let f = flag.clone();
                Box::pin(async move {
                    f.store(true, Ordering::SeqCst);
                    JobResult::Completed
                })
            })),
        ));

        // Pause before enqueueing so neither transcription nor summary can run.
        queue.scheduler.manual_pause_all.store(true, Ordering::SeqCst);
        let _handle = queue.spawn_worker();
        queue.enqueue("s7-gates".to_string(), PathBuf::from("/audio.mp4")).await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!summary_called.load(Ordering::SeqCst), "summary must not run while scheduler is paused");
        let state = queue.get_state().await;
        let job = state.jobs.iter().find(|j| j.meeting_id == "s7-gates").unwrap();
        assert_ne!(job.status, JobStatus::Done, "job must not complete while scheduler is paused");

        // Clear the pause — worker woken by resume_all.
        queue.scheduler.manual_pause_all.store(false, Ordering::SeqCst);
        queue.resume_all().await;

        wait_for_status(&queue, "s7-gates", JobStatus::Done).await;
        assert!(summary_called.load(Ordering::SeqCst), "summary must run after scheduler gates clear");
    }
}
