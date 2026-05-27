import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

export type JobStatus = "Pending" | "InProgress" | "Paused" | "Done" | "Failed";
export type JobPhase = "Transcribing" | "Summarising" | "Diarizing";

/** IndexedDB-normalised snake_case status — single source of truth for persistence. */
export type QueueJobStatus = "pending" | "in_progress" | "paused" | "done" | "failed";

const JOB_STATUS_MAP: Record<JobStatus, QueueJobStatus> = {
  Pending: "pending",
  InProgress: "in_progress",
  Paused: "paused",
  Done: "done",
  Failed: "failed",
};

export function toQueueJobStatus(status: JobStatus): QueueJobStatus {
  return JOB_STATUS_MAP[status];
}

export interface QueueJob {
  meeting_id: string;
  audio_path: string;
  status: JobStatus;
  phase: JobPhase;
  pause_reason?: string | null;
  /** Decorated client-side from retranscription-progress events; not sent by Rust. */
  progress_percent?: number;
}

export interface QueueSnapshot {
  jobs: QueueJob[];
  /** Mirrors scheduler.manual_pause_all so the UI can render Pause vs Resume
   *  without inferring from per-job statuses (which lag in-flight yields). */
  manual_pause_all: boolean;
}

export interface RetranscriptionProgressEvent {
  meeting_id: string;
  stage: string;
  progress_percentage: number;
  message: string;
}

export async function pauseAllBackgroundWork(): Promise<void> {
  await invoke("pause_all_background_work");
}

export async function resumeAllBackgroundWork(): Promise<void> {
  await invoke("resume_all_background_work");
}

export async function getQueueState(): Promise<QueueSnapshot> {
  return invoke<QueueSnapshot>("get_queue_state");
}

export async function cancelQueuedJob(meetingId: string): Promise<void> {
  await invoke("cancel_queued_job", { meetingId });
}

export async function enqueueTranscriptionJob(meetingId: string, audioPath: string): Promise<void> {
  await invoke("enqueue_transcription_job", { meetingId, audioPath });
}

export function onQueueChanged(
  callback: (snapshot: QueueSnapshot) => void
): Promise<UnlistenFn> {
  return listen<QueueSnapshot>("transcription-queue-changed", (event) => {
    callback(event.payload);
  });
}
