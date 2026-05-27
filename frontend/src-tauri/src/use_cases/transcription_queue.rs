// use_cases/transcription_queue.rs
//
// Transcription queue, scheduler, and worker loop.

use crate::use_cases::scheduler_settings::{SchedulerLiveConfig, SchedulingMode};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, Notify};

/// Consecutive 5-second samples required to flip a hysteresis gate (default: 30 s / 5 s = 6).
const DEFAULT_HYSTERESIS_WINDOW: usize = 6;

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

    /// When Some, the scheduler reads thresholds/mode from live config.
    /// When None, falls back to hardcoded defaults (backward compat for tests).
    config: Option<Arc<SchedulerLiveConfig>>,

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
            config: None,
            cpu_window: Arc::new(StdMutex::new(VecDeque::new())),
            ram_window: Arc::new(StdMutex::new(VecDeque::new())),
        }
    }

    pub fn with_config(config: Arc<SchedulerLiveConfig>) -> Self {
        Self {
            recording_busy: Arc::new(AtomicBool::new(false)),
            meeting_busy: Arc::new(AtomicBool::new(false)),
            cpu_busy: Arc::new(AtomicBool::new(false)),
            ram_busy: Arc::new(AtomicBool::new(false)),
            manual_pause_all: Arc::new(AtomicBool::new(false)),
            config: Some(config),
            cpu_window: Arc::new(StdMutex::new(VecDeque::new())),
            ram_window: Arc::new(StdMutex::new(VecDeque::new())),
        }
    }

    pub fn can_run(&self) -> bool {
        if self.manual_pause_all.load(Ordering::Relaxed)
            || self.recording_busy.load(Ordering::Relaxed)
            || self.meeting_busy.load(Ordering::Relaxed)
        {
            return false;
        }

        let mode = self
            .config
            .as_ref()
            .map(|c| c.get_mode())
            .unwrap_or(SchedulingMode::Polite);

        match mode {
            SchedulingMode::Aggressive => true,
            SchedulingMode::Polite => {
                !self.cpu_busy.load(Ordering::Relaxed)
                    && !self.ram_busy.load(Ordering::Relaxed)
            }
            SchedulingMode::Manual => false, // auto-resume disabled; only run_transcription_job_now
        }
    }

    /// True when a forced (user-initiated) run is allowed.
    /// Hard gates (recording, meeting, manual_pause) still apply, but CPU/RAM
    /// and the manual auto-resume gate are skipped.
    pub fn can_run_forced(&self) -> bool {
        if self.manual_pause_all.load(Ordering::Relaxed)
            || self.recording_busy.load(Ordering::Relaxed)
            || self.meeting_busy.load(Ordering::Relaxed)
        {
            return false;
        }
        let mode = self
            .config
            .as_ref()
            .map(|c| c.get_mode())
            .unwrap_or(SchedulingMode::Polite);
        match mode {
            SchedulingMode::Aggressive => true,
            SchedulingMode::Manual => true,
            SchedulingMode::Polite => !self.cpu_busy.load(Ordering::Relaxed)
                && !self.ram_busy.load(Ordering::Relaxed),
        }
    }

    pub fn feed_cpu_sample(&self, pct: f64) {
        let threshold = self
            .config
            .as_ref()
            .map(|c| c.cpu_threshold_pct.load(std::sync::atomic::Ordering::Relaxed) as f64)
            .unwrap_or(70.0);
        let window_size = self.cpu_hysteresis_window();
        self.feed_sample(&self.cpu_window, &self.cpu_busy, pct, threshold, window_size);
    }

    pub fn feed_ram_sample(&self, pct: f64) {
        let threshold = self
            .config
            .as_ref()
            .map(|c| c.ram_threshold_pct.load(std::sync::atomic::Ordering::Relaxed) as f64)
            .unwrap_or(80.0);
        let window_size = self.ram_hysteresis_window();
        self.feed_sample(&self.ram_window, &self.ram_busy, pct, threshold, window_size);
    }

    fn cpu_hysteresis_window(&self) -> usize {
        self.config
            .as_ref()
            .map(|c| {
                let secs = c.cpu_duration_secs.load(std::sync::atomic::Ordering::Relaxed);
                (secs.max(5) / 5) as usize
            })
            .unwrap_or(DEFAULT_HYSTERESIS_WINDOW)
    }

    fn ram_hysteresis_window(&self) -> usize {
        self.config
            .as_ref()
            .map(|c| {
                let secs = c.ram_duration_secs.load(std::sync::atomic::Ordering::Relaxed);
                (secs.max(5) / 5) as usize
            })
            .unwrap_or(DEFAULT_HYSTERESIS_WINDOW)
    }

    fn feed_sample(
        &self,
        window: &Arc<StdMutex<VecDeque<f64>>>,
        busy: &Arc<AtomicBool>,
        pct: f64,
        threshold: f64,
        window_size: usize,
    ) {
        let mut w = match window.lock() {
            Ok(g) => g,
            Err(e) => {
                log::warn!("scheduler sample window mutex poisoned, skipping sample: {e}");
                return;
            }
        };
        w.push_back(pct);
        while w.len() > window_size {
            w.pop_front();
        }
        if w.len() == window_size {
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

/// Three-phase processing: transcription, then (optionally) summary, then diarization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobPhase {
    Transcribing,
    Summarising,
    Diarizing,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub jobs: Vec<Job>,
    /// Manual-pause-all flag from the scheduler. Exposed on every snapshot so the
    /// frontend toggle can render Pause vs Resume without inferring from per-job
    /// statuses (which is unreliable: an in-flight job stays InProgress until it
    /// yields at the next chunk boundary, masking the user's intent).
    pub manual_pause_all: bool,
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
    /// Set to `Some` when diarization models are available.
    diarization_processor: Option<ProcessorFn>,
    /// Meeting IDs that have been force-started via run_job_now in manual mode.
    /// The worker loop checks this to bypass the manual-mode gate for those jobs.
    forced_meetings: Arc<Mutex<Vec<String>>>,
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
            diarization_processor: None,
            forced_meetings: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_all_processors(
        processor: ProcessorFn,
        summary_processor: Option<ProcessorFn>,
        diarization_processor: Option<ProcessorFn>,
    ) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
            scheduler: Arc::new(Scheduler::new()),
            processor,
            summary_processor,
            diarization_processor,
            forced_meetings: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_processors_and_config(
        processor: ProcessorFn,
        summary_processor: Option<ProcessorFn>,
        config: Arc<SchedulerLiveConfig>,
    ) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
            scheduler: Arc::new(Scheduler::with_config(config)),
            processor,
            summary_processor,
            diarization_processor: None,
            forced_meetings: Arc::new(Mutex::new(Vec::new())),
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
            pause_reason: None,
        });
        self.notify.notify_one();
    }

    /// Wake the worker loop so it re-evaluates `can_run()` after a gate change.
    pub fn wake_worker(&self) {
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
        {
            let mut forced = self.forced_meetings.lock().await;
            forced.retain(|id| id != meeting_id);
        }
        if was_in_progress {
            crate::audio::retranscription::cancel_retranscription();
        }
    }

    /// Force-start a specific pending/paused job, bypassing the manual-mode
    /// auto-resume gate.  The job still respects recording_active,
    /// meeting_detected, and manual_pause at chunk boundaries.
    pub async fn run_job_now(&self, meeting_id: &str) -> bool {
        if !self.scheduler.can_run_forced() {
            return false;
        }
        let found = {
            let mut jobs = self.jobs.lock().await;
            jobs.iter_mut().any(|j| {
                if j.meeting_id == meeting_id
                    && (j.status == JobStatus::Pending || j.status == JobStatus::Paused)
                {
                    j.status = JobStatus::Pending;
                    true
                } else {
                    false
                }
            })
        };
        if found {
            {
                let mut forced = self.forced_meetings.lock().await;
                if !forced.contains(&meeting_id.to_string()) {
                    forced.push(meeting_id.to_string());
                }
            }
            self.notify.notify_one();
        }
        found
    }

    /// Pause all pending/in-progress jobs (manual override).
    ///
    /// Sets `scheduler.manual_pause_all` so the worker's `can_run()` blocks new
    /// pickups, and asserts `SHOULD_YIELD` so any in-flight retranscription
    /// chunk exits at its next boundary (per spec post-meeting-pipeline.md:105).
    pub async fn pause_all(&self) {
        self.scheduler.manual_pause_all.store(true, Ordering::Relaxed);
        {
            let mut jobs = self.jobs.lock().await;
            for job in jobs.iter_mut() {
                if job.status == JobStatus::Pending || job.status == JobStatus::InProgress {
                    job.status = JobStatus::Paused;
                    job.pause_reason = Some("manual".to_string());
                }
            }
        }
        SHOULD_YIELD.store(true, Ordering::SeqCst);
    }

    /// Resume all paused jobs and wake the worker.
    ///
    /// Clears `SHOULD_YIELD` (set by `pause_all`) and `scheduler.manual_pause_all`
    /// before flipping paused jobs back to pending, so the worker's next tick
    /// sees a clean state and can pick them up.
    pub async fn resume_all(&self) {
        SHOULD_YIELD.store(false, Ordering::SeqCst);
        self.scheduler.manual_pause_all.store(false, Ordering::Relaxed);
        {
            let mut jobs = self.jobs.lock().await;
            for job in jobs.iter_mut() {
                if job.status == JobStatus::Paused {
                    job.status = JobStatus::Pending;
                    job.pause_reason = None;
                }
            }
        }
        self.notify.notify_one();
    }

    pub async fn get_state(&self) -> QueueSnapshot {
        let jobs = self.jobs.lock().await;
        QueueSnapshot {
            jobs: jobs.clone(),
            manual_pause_all: self.scheduler.manual_pause_all.load(Ordering::Relaxed),
        }
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
        let diarization_processor = self.diarization_processor.clone();
        let forced_meetings = self.forced_meetings.clone();
        tauri::async_runtime::spawn(worker_loop(
            jobs,
            notify,
            scheduler,
            processor,
            summary_processor,
            diarization_processor,
            notifier,
            forced_meetings,
        ))
    }
}

async fn worker_loop(
    jobs: Arc<Mutex<Vec<Job>>>,
    notify: Arc<Notify>,
    scheduler: Arc<Scheduler>,
    processor: ProcessorFn,
    summary_processor: Option<ProcessorFn>,
    diarization_processor: Option<ProcessorFn>,
    notifier: Option<StateChangeNotifier>,
    forced_meetings: Arc<Mutex<Vec<String>>>,
) {
    loop {
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
        }

        loop {
            // Check if there are any forced meetings waiting to run.
            // In manual mode, can_run() returns false, but forced jobs bypass that.
            let has_forced = {
                let forced = forced_meetings.lock().await;
                !forced.is_empty()
            };

            if !scheduler.can_run() && !has_forced {
                log::debug!(
                    "worker: can_run=false (manual={} recording={} meeting={} cpu={} ram={})",
                    scheduler.manual_pause_all.load(Ordering::Relaxed),
                    scheduler.recording_busy.load(Ordering::Relaxed),
                    scheduler.meeting_busy.load(Ordering::Relaxed),
                    scheduler.cpu_busy.load(Ordering::Relaxed),
                    scheduler.ram_busy.load(Ordering::Relaxed),
                );
                break;
            }

            // Pickup + InProgress transition in a single critical section.
            //
            // Two races are closed here:
            //
            // Race 1 (pre-lock): pause_all() could fire between can_run() and the
            // lock acquisition.  Without re-checking manual_pause_all inside the
            // lock, the worker would flip Pending → InProgress and then reset
            // SHOULD_YIELD, clobbering pause_all's assertion.
            //
            // Race 2 (last-chunk): even with Race 1 closed, pause_all can fire
            // while the processor is on its final chunk.  The processor checks
            // SHOULD_YIELD before each chunk, not after; if the last chunk began
            // before the flag was set, the processor returns Completed and the
            // match arm below must not overwrite a Paused status → Done.
            //
            // Part A (this section): reset SHOULD_YIELD inside the lock, in the
            // same critical section as the InProgress flip, so pause_all's
            // subsequent SeqCst store always lands after ours.
            // Part B: the match arm checks j.status == InProgress before writing
            // terminal states, so a Paused job set by pause_all is never clobbered.
            let job_info: Option<(String, PathBuf, JobPhase)> = {
                let mut jobs_guard = jobs.lock().await;
                let mut forced = forced_meetings.lock().await;
                let manual_paused = scheduler.manual_pause_all.load(Ordering::Relaxed);

                // In manual mode with forced meetings, pick up the forced job specifically.
                let forced_pickup = if !forced.is_empty() {
                    let forced_id = &forced[0];
                    let found = jobs_guard.iter_mut().find(|j| {
                        j.meeting_id == *forced_id
                            && (j.status == JobStatus::Pending || j.status == JobStatus::Paused)
                            && !manual_paused
                    });
                    if let Some(j) = found {
                        let info = (j.meeting_id.clone(), j.audio_path.clone(), j.phase.clone());
                        j.status = JobStatus::InProgress;
                        j.pause_reason = None;
                        SHOULD_YIELD.store(false, Ordering::SeqCst);
                        forced.remove(0);
                        Some(info)
                    } else {
                        // Forced job no longer eligible (e.g. cancelled or still paused)
                        forced.remove(0);
                        None
                    }
                } else {
                    None
                };

                if forced_pickup.is_some() {
                    let snapshot = QueueSnapshot {
                        jobs: jobs_guard.clone(),
                        manual_pause_all: manual_paused,
                    };
                    drop(jobs_guard);
                    drop(forced);
                    if let Some(n) = &notifier { n(snapshot); }
                    forced_pickup
                } else {
                    // Normal path (polite/aggressive modes, or manual with no forced jobs)
                    let found = jobs_guard
                        .iter_mut()
                        .find(|j| j.status == JobStatus::Pending);
                    let pending_exists = found.is_some();
                    let info = if let Some(j) = found {
                        if manual_paused {
                            j.status = JobStatus::Paused;
                            j.pause_reason = Some("manual".to_string());
                            None
                        } else {
                            let info = (j.meeting_id.clone(), j.audio_path.clone(), j.phase.clone());
                            j.status = JobStatus::InProgress;
                            j.pause_reason = None;
                            SHOULD_YIELD.store(false, Ordering::SeqCst);
                            Some(info)
                        }
                    } else {
                        None
                    };
                    let snapshot = QueueSnapshot {
                        jobs: jobs_guard.clone(),
                        manual_pause_all: manual_paused,
                    };
                    drop(jobs_guard);
                    drop(forced);
                    if let Some(n) = &notifier { n(snapshot); }
                    if info.is_none() && !pending_exists {
                        break;
                    }
                    info
                }
            };

            let Some((meeting_id, audio_path, phase)) = job_info else {
                continue;
            };

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
                JobPhase::Diarizing => {
                    if let Some(ref dp) = diarization_processor {
                        (dp)(meeting_id.clone(), audio_path).await
                    } else {
                        JobResult::Completed
                    }
                }
            };

            // Update status / phase based on result.
            let snapshot = {
                let mut jobs = jobs.lock().await;
                let mut forced = forced_meetings.lock().await;
                if let Some(j) = jobs.iter_mut().find(|j| j.meeting_id == meeting_id) {
                    match result {
                        // Part B: only write Done/Failed if the job is still InProgress.
                        // If pause_all landed while the processor ran its last chunk,
                        // the job is already Paused and must not be overwritten.
                        JobResult::Completed => {
                            if j.status == JobStatus::InProgress {
                                j.status = JobStatus::Done;
                            }
                        }
                        JobResult::CompletedChain => {
                            if j.status == JobStatus::InProgress {
                                if j.phase == JobPhase::Transcribing
                                    && summary_processor.is_some()
                                {
                                    j.phase = JobPhase::Summarising;
                                    j.status = JobStatus::Pending;
                                    if !forced.contains(&meeting_id) {
                                        forced.push(meeting_id.clone());
                                    }
                                } else if j.phase == JobPhase::Transcribing
                                    && summary_processor.is_none()
                                    && diarization_processor.is_some()
                                {
                                    // No summary — chain directly to diarization
                                    j.phase = JobPhase::Diarizing;
                                    j.status = JobStatus::Pending;
                                    if !forced.contains(&meeting_id) {
                                        forced.push(meeting_id.clone());
                                    }
                                } else if j.phase == JobPhase::Summarising
                                    && diarization_processor.is_some()
                                {
                                    // After summary, chain to diarization
                                    j.phase = JobPhase::Diarizing;
                                    j.status = JobStatus::Pending;
                                    if !forced.contains(&meeting_id) {
                                        forced.push(meeting_id.clone());
                                    }
                                } else {
                                    j.status = JobStatus::Done;
                                }
                            }
                        }
                        JobResult::Yielded => {
                            // Guard matches Part B: a stale Yielded arriving after resume_all
                            // already flipped the job to Pending must not re-pause it.
                            if j.status == JobStatus::InProgress {
                                j.status = JobStatus::Paused;
                                j.pause_reason = Some(infer_pause_reason(scheduler.as_ref()));
                            }
                        }
                        JobResult::Failed(_) => {
                            if j.status == JobStatus::InProgress {
                                j.status = JobStatus::Failed;
                            }
                        }
                    }
                }
                QueueSnapshot {
                    jobs: jobs.clone(),
                    manual_pause_all: scheduler.manual_pause_all.load(Ordering::Relaxed),
                }
            };
            if let Some(n) = &notifier { n(snapshot); }
        }
    }
}

fn infer_pause_reason(scheduler: &Scheduler) -> String {
    if scheduler.recording_busy.load(Ordering::Relaxed) {
        return "recording_active".to_string();
    }
    if scheduler.meeting_busy.load(Ordering::Relaxed) {
        return "meeting_detected".to_string();
    }
    if scheduler.cpu_busy.load(Ordering::Relaxed) {
        return "cpu_high".to_string();
    }
    if scheduler.ram_busy.load(Ordering::Relaxed) {
        return "ram_high".to_string();
    }
    if scheduler.manual_pause_all.load(Ordering::Relaxed) {
        return "manual".to_string();
    }
    "unknown".to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::scheduler_settings::{SchedulerLiveConfig, SchedulerSettings};
    use serial_test::serial;
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
        let folder_id = "Meeting 2026-01-01_00-00-05_2026-01-01_00-00".to_string();

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
        for _ in 0..DEFAULT_HYSTERESIS_WINDOW {
            sched.feed_cpu_sample(80.0);
        }
        assert!(!sched.can_run(), "scheduler must pause after sustained CPU > 70%");
        for _ in 0..DEFAULT_HYSTERESIS_WINDOW {
            sched.feed_cpu_sample(50.0);
        }
        assert!(sched.can_run(), "scheduler must resume after sustained CPU ≤ 70%");
    }

    #[test]
    fn scheduler_pauses_on_sustained_ram_load() {
        let sched = Scheduler::new();
        for _ in 0..DEFAULT_HYSTERESIS_WINDOW {
            sched.feed_ram_sample(85.0);
        }
        assert!(!sched.can_run(), "scheduler must pause after sustained RAM > 80%");
        for _ in 0..DEFAULT_HYSTERESIS_WINDOW {
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
    async fn summary_chain_routing() {
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

        // Edge case: CompletedChain returned but no summary_processor registered → Done, no panic.
        let queue3 = Arc::new(TranscriptionQueue::with_processors(
            Arc::new(|_id, _path| Box::pin(async { JobResult::CompletedChain })),
            None,
        ));
        let _handle3 = queue3.spawn_worker();
        queue3.enqueue("s7-chain-no-proc".to_string(), PathBuf::from("/audio.mp4")).await;
        wait_for_status(&queue3, "s7-chain-no-proc", JobStatus::Done).await;
        let state3 = queue3.get_state().await;
        let job3 = state3.jobs.iter().find(|j| j.meeting_id == "s7-chain-no-proc").unwrap();
        assert_eq!(job3.phase, JobPhase::Transcribing, "CompletedChain with no processor must go to Done in Transcribing phase");
    }

    #[tokio::test]
    #[serial]
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

        // Clear the pause; resume_all() calls notify_one() to wake the sleeping worker.
        // The job is already Pending so no Paused→Pending flip is needed here — the
        // notify is what matters.
        queue.scheduler.manual_pause_all.store(false, Ordering::SeqCst);
        queue.resume_all().await;

        wait_for_status(&queue, "s7-gates", JobStatus::Done).await;
        assert!(summary_called.load(Ordering::SeqCst), "summary must run after scheduler gates clear");
    }

    // ── Manual pause / resume regression (smoke test 12.5 follow-up) ─────────
    //
    // Spec post-meeting-pipeline.md:105 — manual pause must yield in-flight jobs
    // at the next chunk boundary AND prevent new jobs from being picked up.
    // Previous bug: pause_all() flipped statuses but did NOT assert SHOULD_YIELD,
    // so the currently-running retranscription chunk drove to completion and the
    // job ended up Done instead of Paused.

    #[tokio::test]
    #[serial]
    async fn pause_all_asserts_should_yield_for_in_flight_chunk() {
        // Reset before the test — other tests in this file leave SHOULD_YIELD
        // in arbitrary state, and this assertion is order-sensitive.
        SHOULD_YIELD.store(false, Ordering::SeqCst);
        let queue = TranscriptionQueue::new();
        assert!(!SHOULD_YIELD.load(Ordering::SeqCst), "pre-condition: SHOULD_YIELD must start cleared");
        queue.pause_all().await;
        assert!(
            SHOULD_YIELD.load(Ordering::SeqCst),
            "pause_all must assert SHOULD_YIELD so any in-flight retranscription yields at the next chunk boundary"
        );
    }

    #[tokio::test]
    #[serial]
    async fn resume_all_clears_should_yield() {
        SHOULD_YIELD.store(true, Ordering::SeqCst);
        let queue = TranscriptionQueue::new();
        queue.resume_all().await;
        assert!(
            !SHOULD_YIELD.load(Ordering::SeqCst),
            "resume_all must clear SHOULD_YIELD so the next worker tick is not seen as a yield request"
        );
    }

    #[tokio::test]
    #[serial]
    async fn pause_all_sets_manual_pause_all_flag() {
        let queue = TranscriptionQueue::new();
        assert!(!queue.scheduler.manual_pause_all.load(Ordering::SeqCst));
        queue.pause_all().await;
        assert!(
            queue.scheduler.manual_pause_all.load(Ordering::SeqCst),
            "pause_all must set scheduler.manual_pause_all so can_run() blocks subsequent pickups"
        );
        queue.resume_all().await;
        assert!(
            !queue.scheduler.manual_pause_all.load(Ordering::SeqCst),
            "resume_all must clear scheduler.manual_pause_all"
        );
    }

    /// Regression: meeting-end should not lift a deliberate manual pause.
    /// The fix in lib.rs guards the resume_all() call with
    /// `!manual_pause_all`; this test verifies the invariant holds at the
    /// queue level — clearing meeting_busy alone leaves manual_pause_all set.
    #[tokio::test]
    #[serial]
    async fn meeting_end_preserves_manual_pause_when_active() {
        SHOULD_YIELD.store(false, Ordering::SeqCst);
        let queue = TranscriptionQueue::new();

        // User deliberately pauses all background work.
        queue.pause_all().await;
        assert!(queue.scheduler.manual_pause_all.load(Ordering::SeqCst));

        // A meeting is detected (sets meeting_busy).
        queue.scheduler.meeting_busy.store(true, Ordering::SeqCst);
        assert!(!queue.scheduler.can_run(), "must be blocked by both gates");

        // Meeting ends: lib.rs clears meeting_busy but skips resume_all()
        // because manual_pause_all is set.
        queue.scheduler.meeting_busy.store(false, Ordering::SeqCst);
        // (resume_all() is NOT called here — that's the fix being tested)

        assert!(
            queue.scheduler.manual_pause_all.load(Ordering::SeqCst),
            "manual_pause_all must survive meeting-end when user deliberately paused"
        );
        assert!(
            !queue.scheduler.can_run(),
            "can_run() must remain false — manual pause is still active"
        );

        // Cleanup.
        queue.resume_all().await;
    }

    #[tokio::test]
    #[serial]
    async fn snapshot_exposes_manual_pause_all_flag() {
        let queue = TranscriptionQueue::new();
        let snap = queue.get_state().await;
        assert!(!snap.manual_pause_all, "fresh snapshot must report manual_pause_all=false");
        queue.pause_all().await;
        let snap = queue.get_state().await;
        assert!(snap.manual_pause_all, "snapshot after pause_all must report manual_pause_all=true so the UI can render Resume");
        queue.resume_all().await;
        let snap = queue.get_state().await;
        assert!(!snap.manual_pause_all, "snapshot after resume_all must report manual_pause_all=false");
    }

    // Regression for the pickup-window race surfaced in code review.
    //
    // This test covers the deterministic pre-pause path: manual_pause_all is set
    // before the worker spawns, so can_run() returns false and the worker never
    // reaches the critical section. It verifies the processor is not dispatched
    // and the job stays Pending (not InProgress / Done).
    //
    // Note: the narrower race where pause_all() fires after can_run() returns true
    // but before the lock is acquired is not deterministically exercisable without
    // test hooks. That path is closed by Part A (SHOULD_YIELD reset inside lock)
    // and Part B (match-arm InProgress guard). See the comment above worker_loop.
    #[tokio::test]
    #[serial]
    async fn worker_marks_pending_job_paused_under_manual_pause_without_dispatching() {
        SHOULD_YIELD.store(false, Ordering::SeqCst);

        let processor_called = Arc::new(AtomicBool::new(false));
        let pc = processor_called.clone();
        let processor: ProcessorFn = Arc::new(move |_id, _path| {
            let pc = pc.clone();
            Box::pin(async move {
                pc.store(true, Ordering::SeqCst);
                JobResult::Completed
            })
        });

        let queue = Arc::new(TranscriptionQueue::with_processor(processor));
        // Set manual_pause_all=true BEFORE the worker can run can_run().
        queue.pause_all().await;
        let _handle = queue.spawn_worker();
        // Enqueue while paused. The worker's outer loop will tick on notify_one
        // (from enqueue) and enter the inner loop; can_run() will return false
        // and break before reaching pickup. After can_run() blocks, the only
        // way for the job to ever transition out of Pending is via resume_all.
        queue
            .enqueue("paused-pickup".to_string(), PathBuf::from("/audio.mp4"))
            .await;

        // Give the worker plenty of opportunity to (incorrectly) dispatch.
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            !processor_called.load(Ordering::SeqCst),
            "processor must not run while manual_pause_all is set"
        );
        let state = queue.get_state().await;
        let job = state.jobs.iter().find(|j| j.meeting_id == "paused-pickup").unwrap();
        assert_eq!(
            job.status,
            JobStatus::Pending,
            "job enqueued while paused must remain Pending until resume_all clears the gate"
        );
        assert!(state.manual_pause_all, "snapshot must still report manual_pause_all=true");
    }

    // Coverage note: the stale-Yielded race (resume_all promotes a job to Pending
    // while the worker's match arm holds a JobResult::Yielded for that same job)
    // is not deterministically exercisable without test hooks that interpose between
    // the processor return and the match-arm lock acquisition.  The guard on
    // `j.status == InProgress` in the Yielded arm (same as Completed/Failed) is
    // correct by inspection — if the job was already flipped to Pending by
    // resume_all, the guard discards the stale Yielded result and the job stays
    // Pending.  Same rationale applies to Completed/CompletedChain/Failed arms.

    // ── Task 2.1: scheduler reads thresholds from settings ────────────────────

    #[test]
    fn scheduler_reads_thresholds_from_settings() {
        // CPU=40%/15s → window of 3 samples (15/5)
        let settings = SchedulerSettings {
            scheduling_mode: "polite".to_string(),
            cpu_pause_threshold_pct: 40,
            cpu_pause_duration_secs: 15,
            ram_pause_threshold_pct: 80,
            ram_pause_duration_secs: 30,
        };
        let config = Arc::new(SchedulerLiveConfig::from_settings(&settings));
        let sched = Scheduler::with_config(config);

        // With default thresholds (70%/30s), 45% would be fine.
        // With custom CPU threshold of 40%, 45% should trigger busy after 3 samples.
        for _ in 0..3 {
            sched.feed_cpu_sample(45.0);
        }
        assert!(
            sched.cpu_busy.load(Ordering::Relaxed),
            "cpu_busy must be true after 3 samples at 45% with threshold 40%"
        );
    }

    // ── Task 2.2: aggressive mode disables CPU and RAM gates ──────────────────

    #[test]
    fn scheduler_aggressive_mode_disables_cpu_and_ram_gates() {
        let settings = SchedulerSettings {
            scheduling_mode: "aggressive".to_string(),
            cpu_pause_threshold_pct: 70,
            cpu_pause_duration_secs: 30,
            ram_pause_threshold_pct: 80,
            ram_pause_duration_secs: 30,
        };
        let config = Arc::new(SchedulerLiveConfig::from_settings(&settings));
        let sched = Scheduler::with_config(config);

        // Even with CPU/RAM high, can_run() should return true in aggressive mode
        sched.cpu_busy.store(true, Ordering::Relaxed);
        sched.ram_busy.store(true, Ordering::Relaxed);
        assert!(
            sched.can_run(),
            "aggressive mode must allow running despite CPU and RAM gates"
        );
    }

    // ── Task 2.3: manual mode does not auto-resume ───────────────────────────

    #[tokio::test]
    #[serial]
    async fn scheduler_manual_mode_does_not_auto_resume() {
        let settings = SchedulerSettings {
            scheduling_mode: "manual".to_string(),
            cpu_pause_threshold_pct: 70,
            cpu_pause_duration_secs: 30,
            ram_pause_threshold_pct: 80,
            ram_pause_duration_secs: 30,
        };
        let config = Arc::new(SchedulerLiveConfig::from_settings(&settings));
        let sched = Scheduler::with_config(config);

        // In manual mode, can_run() must return false even when all gates are clear
        assert!(
            !sched.can_run(),
            "manual mode must block auto-resume regardless of gates"
        );

        // can_run_forced() must still return true (used by run_job_now)
        assert!(
            sched.can_run_forced(),
            "manual mode must allow forced runs when gates are clear"
        );

        // Forced runs in manual mode must ignore CPU/RAM — the user explicitly chose to run.
        sched.cpu_busy.store(true, Ordering::Relaxed);
        sched.ram_busy.store(true, Ordering::Relaxed);
        assert!(
            sched.can_run_forced(),
            "manual mode forced run must ignore CPU/RAM gates"
        );
    }

    // ── Task 3.2: run_job_now triggers worker even in manual mode ─────────────

    #[tokio::test]
    #[serial]
    async fn run_job_now_triggers_in_manual_mode() {
        SHOULD_YIELD.store(false, Ordering::SeqCst);

        let settings = SchedulerSettings {
            scheduling_mode: "manual".to_string(),
            cpu_pause_threshold_pct: 70,
            cpu_pause_duration_secs: 30,
            ram_pause_threshold_pct: 80,
            ram_pause_duration_secs: 30,
        };
        let config = Arc::new(SchedulerLiveConfig::from_settings(&settings));

        let processor_called = Arc::new(AtomicBool::new(false));
        let pc = processor_called.clone();
        let processor: ProcessorFn = Arc::new(move |_id, _path| {
            let pc = pc.clone();
            Box::pin(async move {
                pc.store(true, Ordering::SeqCst);
                JobResult::Completed
            })
        });

        // Build queue with manual-mode scheduler
        let queue = Arc::new(TranscriptionQueue {
            jobs: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
            scheduler: Arc::new(Scheduler::with_config(config)),
            processor,
            summary_processor: None,
            diarization_processor: None,
            forced_meetings: Arc::new(Mutex::new(Vec::new())),
        });

        queue.enqueue("manual-job".to_string(), PathBuf::from("/audio.mp4")).await;
        let _handle = queue.spawn_worker();

        // Give the worker time to (not) pick up the job
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !processor_called.load(Ordering::SeqCst),
            "processor must not run in manual mode without run_job_now"
        );

        // Now force-run
        let started = queue.run_job_now("manual-job").await;
        assert!(started, "run_job_now must return true for a pending job");

        wait_for_status(&queue, "manual-job", JobStatus::Done).await;
        assert!(
            processor_called.load(Ordering::SeqCst),
            "processor must run after run_job_now in manual mode"
        );
    }

    /// Regression: in manual mode, CompletedChain must auto-chain to the summary
    /// phase without requiring another run_job_now click.
    #[tokio::test]
    #[serial]
    async fn manual_mode_completed_chain_auto_chains_to_summary() {
        SHOULD_YIELD.store(false, Ordering::SeqCst);

        let settings = SchedulerSettings {
            scheduling_mode: "manual".to_string(),
            cpu_pause_threshold_pct: 70,
            cpu_pause_duration_secs: 30,
            ram_pause_threshold_pct: 80,
            ram_pause_duration_secs: 30,
        };
        let config = Arc::new(SchedulerLiveConfig::from_settings(&settings));

        let summary_called = Arc::new(AtomicBool::new(false));
        let flag = summary_called.clone();

        let queue = Arc::new(TranscriptionQueue {
            jobs: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
            scheduler: Arc::new(Scheduler::with_config(config)),
            processor: Arc::new(|_id, _path| {
                Box::pin(async { JobResult::CompletedChain })
            }),
            summary_processor: Some(Arc::new(move |_id, _path| {
                let f = flag.clone();
                Box::pin(async move {
                    f.store(true, Ordering::SeqCst);
                    JobResult::Completed
                })
            })),
            diarization_processor: None,
            forced_meetings: Arc::new(Mutex::new(Vec::new())),
        });

        queue.enqueue("chain-manual".to_string(), PathBuf::from("/audio.mp4")).await;
        let _handle = queue.spawn_worker();

        let started = queue.run_job_now("chain-manual").await;
        assert!(started);

        wait_for_status(&queue, "chain-manual", JobStatus::Done).await;
        assert!(
            summary_called.load(Ordering::SeqCst),
            "summary must run automatically after transcription in manual mode"
        );
    }

    // ── Task 3.2: run_job_now still pauses when recording is active ───────────

    #[tokio::test]
    #[serial]
    async fn run_job_now_respects_recording_gate() {
        SHOULD_YIELD.store(false, Ordering::SeqCst);

        let settings = SchedulerSettings {
            scheduling_mode: "manual".to_string(),
            cpu_pause_threshold_pct: 70,
            cpu_pause_duration_secs: 30,
            ram_pause_threshold_pct: 80,
            ram_pause_duration_secs: 30,
        };
        let config = Arc::new(SchedulerLiveConfig::from_settings(&settings));

        let queue = Arc::new(TranscriptionQueue {
            jobs: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
            scheduler: Arc::new(Scheduler::with_config(config)),
            processor: noop_processor(),
            summary_processor: None,
            diarization_processor: None,
            forced_meetings: Arc::new(Mutex::new(Vec::new())),
        });

        queue.enqueue("manual-gated".to_string(), PathBuf::from("/audio.mp4")).await;

        // Set recording_busy — should block run_job_now
        queue.scheduler.recording_busy.store(true, Ordering::SeqCst);
        let started = queue.run_job_now("manual-gated").await;
        assert!(!started, "run_job_now must return false when recording is active");
    }

    /// Regression: cancel() must remove the meeting from forced_meetings so
    /// the worker loop doesn't try to pick up a cancelled job on its next tick.
    #[tokio::test]
    #[serial]
    async fn cancel_removes_from_forced_meetings() {
        let queue = Arc::new(TranscriptionQueue::with_processors(
            noop_processor(),
            None,
        ));
        queue.enqueue("cancel-forced".to_string(), PathBuf::from("/audio.mp4")).await;
        queue.run_job_now("cancel-forced").await;

        // Verify it's in forced_meetings
        {
            let forced = queue.forced_meetings.lock().await;
            assert!(forced.contains(&"cancel-forced".to_string()));
        }

        queue.cancel("cancel-forced").await;

        let forced = queue.forced_meetings.lock().await;
        assert!(!forced.contains(&"cancel-forced".to_string()),
            "cancel must clean up forced_meetings entry");
    }

    // ── Diarizing phase tests (Group 8) ────────────────────────────────────────

    #[test]
    fn diarizing_phase_serialization_round_trip() {
        let phase = JobPhase::Diarizing;
        let json = serde_json::to_string(&phase).unwrap();
        let parsed: JobPhase = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, JobPhase::Diarizing);
    }

    #[tokio::test]
    #[serial]
    async fn summarising_chains_to_diarizing() {
        SHOULD_YIELD.store(false, Ordering::SeqCst);

        let diarization_called = Arc::new(AtomicBool::new(false));
        let d_flag = diarization_called.clone();

        let queue = Arc::new(TranscriptionQueue::with_all_processors(
            Arc::new(|_id, _path| Box::pin(async { JobResult::CompletedChain })),
            Some(Arc::new(|_id, _path| Box::pin(async { JobResult::CompletedChain }))),
            Some(Arc::new(move |_id, _path| {
                let f = d_flag.clone();
                Box::pin(async move {
                    f.store(true, Ordering::SeqCst);
                    JobResult::Completed
                })
            })),
        ));

        let _handle = queue.spawn_worker();
        queue.enqueue("dia-chain".to_string(), PathBuf::from("/audio.mp4")).await;
        wait_for_status(&queue, "dia-chain", JobStatus::Done).await;

        assert!(diarization_called.load(Ordering::SeqCst), "diarization must run after summary");

        let state = queue.get_state().await;
        let job = state.jobs.iter().find(|j| j.meeting_id == "dia-chain").unwrap();
        assert_eq!(job.phase, JobPhase::Diarizing, "job must end in Diarizing phase after chaining");
    }

    #[tokio::test]
    #[serial]
    async fn transcribing_chains_directly_to_diarizing_when_no_summary() {
        SHOULD_YIELD.store(false, Ordering::SeqCst);

        let diarization_called = Arc::new(AtomicBool::new(false));
        let d_flag = diarization_called.clone();

        let queue = Arc::new(TranscriptionQueue::with_all_processors(
            Arc::new(|_id, _path| Box::pin(async { JobResult::CompletedChain })),
            None,
            Some(Arc::new(move |_id, _path| {
                let f = d_flag.clone();
                Box::pin(async move {
                    f.store(true, Ordering::SeqCst);
                    JobResult::Completed
                })
            })),
        ));

        let _handle = queue.spawn_worker();
        queue.enqueue("dia-direct".to_string(), PathBuf::from("/audio.mp4")).await;
        wait_for_status(&queue, "dia-direct", JobStatus::Done).await;

        assert!(diarization_called.load(Ordering::SeqCst), "diarization must run directly after transcription when no summary");
    }

    #[tokio::test]
    #[serial]
    async fn diarizing_phase_respects_scheduler_gates() {
        SHOULD_YIELD.store(false, Ordering::SeqCst);

        let diarization_called = Arc::new(AtomicBool::new(false));
        let d_flag = diarization_called.clone();

        let queue = Arc::new(TranscriptionQueue::with_all_processors(
            Arc::new(|_id, _path| Box::pin(async { JobResult::Completed })),
            None,
            Some(Arc::new(move |_id, _path| {
                let f = d_flag.clone();
                Box::pin(async move {
                    f.store(true, Ordering::SeqCst);
                    JobResult::Completed
                })
            })),
        ));

        queue.scheduler.recording_busy.store(true, Ordering::SeqCst);
        let _handle = queue.spawn_worker();
        queue.enqueue("dia-gated".to_string(), PathBuf::from("/audio.mp4")).await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!diarization_called.load(Ordering::SeqCst), "diarization must not run while scheduler is blocked");

        queue.scheduler.recording_busy.store(false, Ordering::SeqCst);
        queue.resume_all().await;

        wait_for_status(&queue, "dia-gated", JobStatus::Done).await;
    }

    #[tokio::test]
    #[serial]
    async fn snapshot_includes_diarizing_phase() {
        let queue = Arc::new(TranscriptionQueue::with_all_processors(
            noop_processor(),
            None,
            None,
        ));
        queue.enqueue("dia-snap".to_string(), PathBuf::from("/audio.mp4")).await;

        // Manually set phase for testing
        {
            let mut jobs = queue.jobs.lock().await;
            if let Some(j) = jobs.iter_mut().find(|j| j.meeting_id == "dia-snap") {
                j.phase = JobPhase::Diarizing;
            }
        }

        let state = queue.get_state().await;
        let job = state.jobs.iter().find(|j| j.meeting_id == "dia-snap").unwrap();
        assert_eq!(job.phase, JobPhase::Diarizing);

        // Verify serialization
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("Diarizing"), "snapshot JSON must include Diarizing phase");
    }
}
