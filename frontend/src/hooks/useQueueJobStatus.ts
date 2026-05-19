/**
 * useQueueJobStatus
 *
 * Subscribes to transcription-queue-changed Tauri events and exposes the
 * current snapshot plus per-meeting job lookup. Also subscribes to
 * retranscription-progress so per-job progress % can be merged into the
 * label (e.g. "Transcribing 34%").
 */
import { useState, useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  getQueueState,
  onQueueChanged,
  QueueSnapshot,
  QueueJob,
  RetranscriptionProgressEvent,
} from '@/services/queueService';

const EMPTY_SNAPSHOT: QueueSnapshot = { jobs: [], manual_pause_all: false };

export function useQueueSnapshot(): QueueSnapshot {
  const [snapshot, setSnapshot] = useState<QueueSnapshot>(EMPTY_SNAPSHOT);

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    // Seed with current state so the UI is accurate before the first event.
    getQueueState().then(setSnapshot).catch(() => {});

    onQueueChanged(setSnapshot).then((fn) => { unlisten = fn; });

    return () => { unlisten?.(); };
  }, []);

  return snapshot;
}

/** Per-meeting progress percentages from retranscription-progress events. */
export function useRetranscriptionProgress(): Record<string, number> {
  const [progress, setProgress] = useState<Record<string, number>>({});

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<RetranscriptionProgressEvent>('retranscription-progress', (event) => {
      const { meeting_id, progress_percentage } = event.payload;
      setProgress((prev) => ({ ...prev, [meeting_id]: progress_percentage }));
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  return progress;
}

export function useQueueJob(meetingId: string | undefined): QueueJob | undefined {
  const snapshot = useQueueSnapshot();
  const progress = useRetranscriptionProgress();
  if (!meetingId) return undefined;
  const job = snapshot.jobs.find(j => j.meeting_id === meetingId);
  if (!job) return undefined;
  const p = progress[meetingId];
  return p !== undefined ? { ...job, progress_percent: p } : job;
}

/** Human-readable label for a queue job status + phase. */
export function queueJobLabel(job: QueueJob): string {
  switch (job.status) {
    case 'Pending':
      return 'Queued';
    case 'InProgress': {
      const phaseLabel = job.phase === 'Summarising' ? 'Summarising' : 'Transcribing';
      return job.progress_percent !== undefined
        ? `${phaseLabel} ${job.progress_percent}%`
        : `${phaseLabel}…`;
    }
    case 'Paused':
      return 'Paused';
    case 'Done':
      return 'Done';
    case 'Failed':
      return 'Failed';
    default:
      return job.status;
  }
}
