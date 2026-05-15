/**
 * useQueueJobStatus
 *
 * Subscribes to transcription-queue-changed Tauri events and exposes the
 * current snapshot plus per-meeting job lookup.
 */
import { useState, useEffect } from 'react';
import { getQueueState, onQueueChanged, QueueSnapshot, QueueJob } from '@/services/queueService';

export function useQueueSnapshot() {
  const [snapshot, setSnapshot] = useState<QueueSnapshot>({ jobs: [] });

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    // Seed with current state so the UI is accurate before the first event.
    getQueueState().then(setSnapshot).catch(() => {});

    onQueueChanged(setSnapshot).then((fn) => { unlisten = fn; });

    return () => { unlisten?.(); };
  }, []);

  return snapshot;
}

export function useQueueJob(meetingId: string | undefined): QueueJob | undefined {
  const snapshot = useQueueSnapshot();
  if (!meetingId) return undefined;
  return snapshot.jobs.find(j => j.meeting_id === meetingId);
}

/** Human-readable label for a queue job status + phase. */
export function queueJobLabel(job: QueueJob): string {
  switch (job.status) {
    case 'Pending':
      return 'Queued';
    case 'InProgress':
      return job.phase === 'Summarising' ? 'Summarising…' : 'Transcribing…';
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
