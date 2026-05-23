/**
 * IndexedDB Service — v2 schema
 *
 * v1: stored live-transcript chunks (meetings + transcripts stores).
 * v2: repurposed as transcription-queue persistence. The transcripts store is
 *     dropped; a new transcription_queue store holds one job row per meeting.
 *     Legacy v1 meeting metadata is preserved in a migration_v1_meetings store
 *     so the one-shot recovery modal can offer it on the first post-upgrade
 *     launch (see useTranscriptRecovery.ts / task 11.2).
 */
import { TranscriptUpdate } from '@/types';
import type { QueueJobStatus } from '@/services/queueService';

// ── v1 schema interfaces (kept for the migration read path) ──────────────────

export interface MeetingMetadata {
  meetingId: string;
  title: string;
  startTime: number;
  lastUpdated: number;
  transcriptCount: number;
  savedToSQLite: boolean;
  folderPath?: string;
}

export interface StoredTranscript {
  id?: number;
  meetingId: string;
  text: string;
  timestamp: string;
  confidence: number;
  sequenceId: number;
  storedAt: number;
  audio_start_time?: number;
  audio_end_time?: number;
  duration?: number;
  [key: string]: unknown;
}

// ── v2 schema: transcription queue ──────────────────────────────────────────

export const VALID_QUEUE_STATUSES: readonly QueueJobStatus[] = [
  'pending',
  'in_progress',
  'paused',
  'done',
  'failed',
] as const;

export interface TranscriptionQueueJob {
  meetingId: string;
  status: QueueJobStatus;
  queuePosition: number;
  pauseReason?: string;
  /** Unix ms timestamp when the job was first written to IndexedDB. */
  enqueuedAt: number;
  /** Absolute path to audio.mp4 on disk — needed to re-enqueue on recovery. */
  audioPath: string;
  startedAt?: number;
  completedAt?: number;
  lastError?: string;
}

// ── Service ──────────────────────────────────────────────────────────────────

class IndexedDBService {
  private db: IDBDatabase | null = null;
  private readonly DB_NAME = 'MeetilyRecoveryDB';
  private readonly DB_VERSION = 2;
  private initPromise: Promise<void> | null = null;

  async init(): Promise<void> {
    if (this.initPromise) return this.initPromise;
    if (this.db) return Promise.resolve();

    this.initPromise = new Promise((resolve, reject) => {
      try {
        const request = indexedDB.open(this.DB_NAME, this.DB_VERSION);

        request.onerror = () => {
          console.error('Failed to open IndexedDB:', request.error);
          reject(request.error);
        };

        request.onsuccess = () => {
          this.db = request.result;
          resolve();
        };

        request.onupgradeneeded = (event) => {
          const db = (event.target as IDBOpenDBRequest).result;
          const tx = (event.target as IDBOpenDBRequest).transaction!;
          const oldVersion = event.oldVersion;

          // ── v1 → v2 migration ────────────────────────────────────────────
          // Preserve v1 meeting metadata in a migration store so the one-shot
          // recovery modal can present them on first post-upgrade launch.
          if (oldVersion === 1) {
            if (db.objectStoreNames.contains('meetings')) {
              const meetingsStore = tx.objectStore('meetings');
              const getAllReq = meetingsStore.getAll();

              getAllReq.onsuccess = () => {
                const legacyMeetings = getAllReq.result as MeetingMetadata[];
                if (legacyMeetings.length > 0) {
                  const migStore = db.createObjectStore('migration_v1_meetings', { keyPath: 'meetingId' });
                  migStore.createIndex('lastUpdated', 'lastUpdated', { unique: false });
                  for (const m of legacyMeetings) {
                    migStore.add(m);
                  }
                }
              };
            }

            // Drop v1 live-transcript stores.
            if (db.objectStoreNames.contains('transcripts')) {
              db.deleteObjectStore('transcripts');
            }
            if (db.objectStoreNames.contains('meetings')) {
              db.deleteObjectStore('meetings');
            }
          }

          // ── Create transcription_queue store (v2, all paths) ─────────────
          if (!db.objectStoreNames.contains('transcription_queue')) {
            const queueStore = db.createObjectStore('transcription_queue', { keyPath: 'meetingId' });
            queueStore.createIndex('status', 'status', { unique: false });
            queueStore.createIndex('queuePosition', 'queuePosition', { unique: false });
          }
        };
      } catch (error) {
        console.error('Exception during IndexedDB initialization:', error);
        reject(error);
      }
    });

    return this.initPromise;
  }

  // ── Queue operations ────────────────────────────────────────────────────────

  async enqueueJob(job: TranscriptionQueueJob): Promise<void> {
    if (!this.db) await this.init();
    const tx = this.db!.transaction(['transcription_queue'], 'readwrite');
    const store = tx.objectStore('transcription_queue');
    return new Promise<void>((resolve, reject) => {
      const req = store.put(job);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
    });
  }

  /**
   * Write a job to the queue store, preserving `enqueuedAt` if the row
   * already exists.  Use this for syncing Tauri queue-changed snapshots to
   * IndexedDB so `enqueuedAt` is set once and never overwritten.
   */
  async upsertQueueJob(job: Omit<TranscriptionQueueJob, 'enqueuedAt'> & { enqueuedAt?: number }): Promise<void> {
    if (!this.db) await this.init();
    const existing = await this.getQueueJob(job.meetingId);
    const record: TranscriptionQueueJob = {
      ...job,
      enqueuedAt: existing?.enqueuedAt ?? job.enqueuedAt ?? Date.now(),
    };
    return this.enqueueJob(record);
  }

  async updateJobStatus(
    meetingId: string,
    status: QueueJobStatus,
    updates?: Partial<Omit<TranscriptionQueueJob, 'meetingId' | 'status'>>,
  ): Promise<void> {
    if (!this.db) await this.init();
    const tx = this.db!.transaction(['transcription_queue'], 'readwrite');
    const store = tx.objectStore('transcription_queue');

    return new Promise<void>((resolve, reject) => {
      const getReq = store.get(meetingId);
      getReq.onsuccess = () => {
        const existing = getReq.result as TranscriptionQueueJob | undefined;
        if (!existing) {
          reject(new Error(`Queue job not found: ${meetingId}`));
          return;
        }
        const updated: TranscriptionQueueJob = { ...existing, status, ...updates };
        const putReq = store.put(updated);
        putReq.onsuccess = () => resolve();
        putReq.onerror = () => reject(putReq.error);
      };
      getReq.onerror = () => reject(getReq.error);
    });
  }

  async getQueueJob(meetingId: string): Promise<TranscriptionQueueJob | null> {
    if (!this.db) await this.init();
    const tx = this.db!.transaction(['transcription_queue'], 'readonly');
    const store = tx.objectStore('transcription_queue');
    return new Promise((resolve, reject) => {
      const req = store.get(meetingId);
      req.onsuccess = () => resolve((req.result as TranscriptionQueueJob) ?? null);
      req.onerror = () => reject(req.error);
    });
  }

  async getPendingQueueJobs(): Promise<TranscriptionQueueJob[]> {
    if (!this.db) await this.init();
    const tx = this.db!.transaction(['transcription_queue'], 'readonly');
    const store = tx.objectStore('transcription_queue');
    return new Promise((resolve, reject) => {
      const req = store.getAll();
      req.onsuccess = () => {
        const all = req.result as TranscriptionQueueJob[];
        resolve(all.filter(j => j.status === 'pending' || j.status === 'in_progress'));
      };
      req.onerror = () => reject(req.error);
    });
  }

  /** Delete and re-open the database. For test isolation only. */
  async resetForTests(): Promise<void> {
    if (this.db) {
      this.db.close();
      this.db = null;
    }
    this.initPromise = null;

    await new Promise<void>((resolve, reject) => {
      const req = indexedDB.deleteDatabase(this.DB_NAME);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
      req.onblocked = () => resolve(); // proceed anyway
    });

    await this.init();
  }

  // ── Legacy v1 recovery (one-shot migration path) ──────────────────────────
  // Used by useTranscriptRecovery.ts until migration_v2_complete is set.

  async getLegacyMeetings(): Promise<MeetingMetadata[]> {
    if (!this.db) await this.init();
    if (!this.db!.objectStoreNames.contains('migration_v1_meetings')) return [];
    const tx = this.db!.transaction(['migration_v1_meetings'], 'readonly');
    const store = tx.objectStore('migration_v1_meetings');
    return new Promise((resolve, reject) => {
      const req = store.getAll();
      req.onsuccess = () => {
        const all = (req.result as MeetingMetadata[]).filter(m => !m.savedToSQLite);
        all.sort((a, b) => b.lastUpdated - a.lastUpdated);
        resolve(all);
      };
      req.onerror = () => reject(req.error);
    });
  }

  // ── Existing v1 methods kept for backward compatibility ──────────────────
  // These read from the migration_v1_meetings store (post-upgrade) or the
  // original meetings store (pre-upgrade, when oldVersion was 1 and this
  // code ran before the migration dropped the store). They will be removed
  // once the migration is complete (task 11).

  async getAllMeetings(): Promise<MeetingMetadata[]> {
    return this.getLegacyMeetings();
  }

  async getMeetingMetadata(meetingId: string): Promise<MeetingMetadata | null> {
    if (!this.db) await this.init();
    const storeName = this.db!.objectStoreNames.contains('migration_v1_meetings')
      ? 'migration_v1_meetings'
      : null;
    if (!storeName) return null;

    const tx = this.db!.transaction([storeName], 'readonly');
    const store = tx.objectStore(storeName);
    return new Promise((resolve, reject) => {
      const req = store.get(meetingId);
      req.onsuccess = () => resolve((req.result as MeetingMetadata) ?? null);
      req.onerror = () => reject(req.error);
    });
  }

  async markMeetingSaved(meetingId: string): Promise<void> {
    if (!this.db) await this.init();
    if (!this.db!.objectStoreNames.contains('migration_v1_meetings')) return;
    const tx = this.db!.transaction(['migration_v1_meetings'], 'readwrite');
    const store = tx.objectStore('migration_v1_meetings');

    return new Promise((resolve, reject) => {
      const getReq = store.get(meetingId);
      getReq.onsuccess = () => {
        const meeting = getReq.result as MeetingMetadata | undefined;
        if (meeting) {
          meeting.savedToSQLite = true;
          meeting.lastUpdated = Date.now();
          const putReq = store.put(meeting);
          putReq.onsuccess = () => resolve();
          putReq.onerror = () => reject(putReq.error);
        } else {
          resolve();
        }
      };
      getReq.onerror = () => reject(getReq.error);
    });
  }

  async deleteMeeting(meetingId: string): Promise<void> {
    if (!this.db) await this.init();
    const stores: string[] = [];
    if (this.db!.objectStoreNames.contains('migration_v1_meetings')) {
      stores.push('migration_v1_meetings');
    }
    if (stores.length === 0) return;

    const tx = this.db!.transaction(stores, 'readwrite');
    await new Promise<void>((resolve, reject) => {
      const req = tx.objectStore('migration_v1_meetings').delete(meetingId);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
    });
  }

  async getTranscripts(meetingId: string): Promise<StoredTranscript[]> {
    // The transcripts store no longer exists in v2. Return empty array so
    // callers don't crash; the legacy recovery modal handles this gracefully.
    void meetingId;
    return [];
  }

  async getTranscriptCount(meetingId: string): Promise<number> {
    void meetingId;
    return 0;
  }

  // saveTranscript removed in task 3.4 — no callers remain after §1 cleanup.

  // v1 cleanup methods — no-ops in v2 (the meetings/transcripts stores no longer exist).
  async deleteOldMeetings(_daysOld: number): Promise<number> { return 0; }
  async deleteSavedMeetings(_hoursOld: number): Promise<number> { return 0; }
}

export const indexedDBService = new IndexedDBService();
