/**
 * useTranscriptRecovery Hook
 *
 * Orchestrates transcript recovery operations for interrupted meetings.
 * v2: reads the transcription_queue IndexedDB store for jobs that were
 * pending or in_progress when the app last closed.  Re-enqueues them
 * via the Tauri queue use case so the background worker re-processes them.
 *
 * The 15 s startup grace window avoids showing jobs that are still actively
 * running in the current session's worker.
 */

import { useState, useCallback } from 'react';
import { indexedDBService, MeetingMetadata, StoredTranscript, TranscriptionQueueJob } from '@/services/indexedDBService';
import { cancelQueuedJob, enqueueTranscriptionJob } from '@/services/queueService';

export interface UseTranscriptRecoveryReturn {
  recoverableMeetings: MeetingMetadata[];
  isLoading: boolean;
  isRecovering: boolean;
  checkForRecoverableTranscripts: () => Promise<void>;
  recoverMeeting: (meetingId: string) => Promise<{ success: boolean; audioRecoveryStatus?: null; meetingId?: string }>;
  loadMeetingTranscripts: (meetingId: string) => Promise<StoredTranscript[]>;
  deleteRecoverableMeeting: (meetingId: string) => Promise<void>;
}

/** Jobs enqueued more than this many ms ago are considered "previous session". */
const STARTUP_GRACE_MS = 15_000;

/**
 * Key persisted in localStorage once the v1→v2 IndexedDB migration has been
 * offered to the user.  After this key is set the legacy recovery path is
 * never shown again.
 */
export const MIGRATION_V2_COMPLETE_KEY = 'migration_v2_complete';

/** Convert a queue job to the MeetingMetadata shape expected by the modal. */
function queueJobToMetadata(job: TranscriptionQueueJob): MeetingMetadata {
  // Derive display title from the meeting folder name (last path segment).
  const segments = job.audioPath.replace(/\\/g, '/').split('/');
  const folderName = segments[segments.length - 2] ?? job.meetingId;
  // folderPath is the parent directory of audio.mp4.
  const folderPath = segments.slice(0, -1).join('/') || undefined;

  return {
    meetingId: job.meetingId,
    title: folderName,
    startTime: job.enqueuedAt,
    lastUpdated: job.startedAt ?? job.enqueuedAt,
    transcriptCount: 0,
    savedToSQLite: false,
    folderPath,
  };
}

export function useTranscriptRecovery(): UseTranscriptRecoveryReturn {
  const [recoverableMeetings, setRecoverableMeetings] = useState<MeetingMetadata[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [isRecovering, setIsRecovering] = useState(false);

  /**
   * Scan for recoverable content:
   * 1. One-shot legacy path — when `migration_v2_complete` is absent in
   *    localStorage AND there are unsaved v1 legacy meetings in IndexedDB,
   *    surface them so the user can save them before we drop the v1 stores.
   *    After the user accepts or dismisses, call `markMigrationComplete()`.
   * 2. Normal path — scan the transcription_queue store for jobs that were
   *    pending or in_progress before the current session started.
   */
  const checkForRecoverableTranscripts = useCallback(async () => {
    setIsLoading(true);
    try {
      // ── Legacy one-shot path (task 11.2) ──────────────────────────────────
      const isMigrationComplete = !!localStorage.getItem(MIGRATION_V2_COMPLETE_KEY);
      if (!isMigrationComplete) {
        const legacyMeetings = await indexedDBService.getLegacyMeetings();
        if (legacyMeetings.length > 0) {
          // Surface legacy meetings; the normal queue path runs after the user
          // accepts/dismisses (recoverMeeting sets migration_v2_complete).
          setRecoverableMeetings(legacyMeetings);
          return;
        }
        // No legacy meetings — mark migration complete so we don't check again.
        localStorage.setItem(MIGRATION_V2_COMPLETE_KEY, '1');
      }

      // ── Normal queue path ─────────────────────────────────────────────────
      const jobs = await indexedDBService.getPendingQueueJobs();
      const now = Date.now();
      const stale = jobs.filter(j => now - j.enqueuedAt > STARTUP_GRACE_MS);
      setRecoverableMeetings(stale.map(queueJobToMetadata));
    } catch (error) {
      console.error('Failed to check for recoverable transcripts:', error);
      setRecoverableMeetings([]);
    } finally {
      setIsLoading(false);
    }
  }, []);

  /**
   * Load transcripts for preview — queue-based jobs have no transcript chunks
   * yet, so this always returns an empty array in the normal recovery path.
   */
  const loadMeetingTranscripts = useCallback(async (_meetingId: string): Promise<StoredTranscript[]> => {
    return [];
  }, []);

  /**
   * Recover a meeting.  Two paths:
   * - Legacy v1 meeting (has folderPath in migration_v1_meetings store): enqueue
   *   via the audio path derived from folderPath.  After the last legacy meeting
   *   is processed, set migration_v2_complete so the one-shot path never fires again.
   * - Normal queue job (exists in transcription_queue store): cancel stale Rust
   *   queue entry and re-enqueue fresh.
   */
  const recoverMeeting = useCallback(async (meetingId: string): Promise<{ success: boolean; audioRecoveryStatus?: null; meetingId?: string }> => {
    setIsRecovering(true);
    try {
      const updatedList = (prev: MeetingMetadata[]) => prev.filter(m => m.meetingId !== meetingId);

      // ── Legacy v1 path ─────────────────────────────────────────────────────
      const legacyMeta = await indexedDBService.getMeetingMetadata(meetingId);
      if (legacyMeta && legacyMeta.folderPath) {
        const audioPath = legacyMeta.folderPath.replace(/\\/g, '/') + '/audio.mp4';

        await cancelQueuedJob(meetingId).catch(() => {/* not in queue — OK */});
        await enqueueTranscriptionJob(meetingId, audioPath);
        await indexedDBService.markMeetingSaved(meetingId);

        setRecoverableMeetings((prev) => {
          const next = updatedList(prev);
          if (next.length === 0) {
            localStorage.setItem(MIGRATION_V2_COMPLETE_KEY, '1');
          }
          return next;
        });

        return { success: true, audioRecoveryStatus: null, meetingId };
      }

      // ── Normal queue path ──────────────────────────────────────────────────
      const job = await indexedDBService.getQueueJob(meetingId);
      if (!job) {
        throw new Error(`No recoverable record found for meetingId: ${meetingId}`);
      }

      await cancelQueuedJob(meetingId).catch(() => {/* not in queue — OK */});
      await enqueueTranscriptionJob(meetingId, job.audioPath);
      await indexedDBService.updateJobStatus(meetingId, 'pending');

      setRecoverableMeetings(updatedList);
      return { success: true, audioRecoveryStatus: null, meetingId };
    } catch (error) {
      console.error('Failed to recover meeting:', error);
      throw error;
    } finally {
      setIsRecovering(false);
    }
  }, []);

  /**
   * Dismiss a recoverable meeting without re-enqueueing.
   */
  const deleteRecoverableMeeting = useCallback(async (meetingId: string): Promise<void> => {
    try {
      // Mark legacy meetings as saved so they vanish from getLegacyMeetings().
      await indexedDBService.markMeetingSaved(meetingId).catch(() => {});
      // Also mark queue jobs as failed so they vanish from getPendingQueueJobs().
      await indexedDBService
        .updateJobStatus(meetingId, 'failed', { lastError: 'Dismissed by user' })
        .catch(() => {});
      setRecoverableMeetings((prev) => {
        const next = prev.filter(m => m.meetingId !== meetingId);
        if (next.length === 0 && !localStorage.getItem(MIGRATION_V2_COMPLETE_KEY)) {
          localStorage.setItem(MIGRATION_V2_COMPLETE_KEY, '1');
        }
        return next;
      });
    } catch (error) {
      console.error('Failed to delete meeting:', error);
      throw error;
    }
  }, []);

  return {
    recoverableMeetings,
    isLoading,
    isRecovering,
    checkForRecoverableTranscripts,
    recoverMeeting,
    loadMeetingTranscripts,
    deleteRecoverableMeeting,
  };
}
