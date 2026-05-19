import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

export type JobStatus = "Pending" | "InProgress" | "Paused" | "Done" | "Failed";
export type JobPhase = "Transcribing" | "Summarising";

export interface QueueJob {
  meeting_id: string;
  audio_path: string;
  status: JobStatus;
  phase: JobPhase;
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
