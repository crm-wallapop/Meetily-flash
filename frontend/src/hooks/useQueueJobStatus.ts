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
import {
  SchedulerSettings,
  defaultSchedulerSettings,
  getSchedulerSettings,
} from '@/services/schedulerSettingsService';

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
    let unlistenProgress: (() => void) | undefined;
    let unlistenQueue: (() => void) | undefined;

    listen<RetranscriptionProgressEvent>('retranscription-progress', (event) => {
      const { meeting_id, progress_percentage } = event.payload;
      setProgress((prev) => ({ ...prev, [meeting_id]: progress_percentage }));
    }).then((fn) => { unlistenProgress = fn; });

    // Prune entries for jobs that are no longer active (Done/Failed/absent).
    onQueueChanged((snapshot) => {
      const activeIds = new Set(
        snapshot.jobs
          .filter(j => j.status !== 'Done' && j.status !== 'Failed')
          .map(j => j.meeting_id),
      );
      setProgress((prev) => {
        const keys = Object.keys(prev);
        if (keys.length === 0) return prev;
        const stale = keys.filter(k => !activeIds.has(k));
        if (stale.length === 0) return prev;
        const next = { ...prev };
        for (const k of stale) delete next[k];
        return next;
      });
    }).then((fn) => { unlistenQueue = fn; });

    return () => { unlistenProgress?.(); unlistenQueue?.(); };
  }, []);

  return progress;
}

export function useSchedulerSettings(): SchedulerSettings {
  const [settings, setSettings] = useState<SchedulerSettings>(defaultSchedulerSettings());
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getSchedulerSettings().then(setSettings).catch(() => {});
    listen<SchedulerSettings>('scheduler-settings-changed', (event) => {
      setSettings(event.payload);
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);
  return settings;
}

export function useQueueJob(meetingId: string | undefined): QueueJob | undefined {
  const snapshot = useQueueSnapshot();
  const progress = useRetranscriptionProgress();
  if (!meetingId) return undefined;
  const job = snapshot.jobs.find(j => j.meeting_id === meetingId);
  if (!job) return undefined;
  const p = progress[meetingId];
  if (p !== undefined && job.phase !== 'Summarising') {
    return { ...job, progress_percent: p };
  }
  return job;
}

/** Format a pause reason with current scheduler settings. */
export function formatPauseReason(
  reason: string | null | undefined,
  settings: SchedulerSettings,
): string {
  switch (reason) {
    case 'recording_active':
      return "Paused — you're recording";
    case 'meeting_detected':
      return "Paused — you're in a meeting";
    case 'cpu_high':
      return `Paused — CPU above ${settings.cpu_pause_threshold_pct} % for ${settings.cpu_pause_duration_secs} s`;
    case 'ram_high':
      return `Paused — RAM above ${settings.ram_pause_threshold_pct} % for ${settings.ram_pause_duration_secs} s`;
    case 'manual':
      return 'Paused — manually';
    default:
      return 'Paused';
  }
}

export function shouldShowRunNow(job: QueueJob, settings: SchedulerSettings): boolean {
  if (settings.scheduling_mode !== 'manual') return false;
  return job.status === 'Pending' || job.status === 'Paused';
}

/** Human-readable label for a queue job status + phase. */
export function queueJobLabel(job: QueueJob, settings?: SchedulerSettings): string {
  switch (job.status) {
    case 'Pending':
      return 'Queued';
    case 'InProgress': {
      if (job.phase === 'Summarising') {
        return 'Summarising…';
      }
      return job.progress_percent !== undefined
        ? `Transcribing ${job.progress_percent}%`
        : 'Transcribing…';
    }
    case 'Paused':
      if (job.pause_reason && settings) {
        return formatPauseReason(job.pause_reason, settings);
      }
      return 'Paused';
    case 'Done':
      return 'Done';
    case 'Failed':
      return 'Failed';
    default:
      return job.status;
  }
}
