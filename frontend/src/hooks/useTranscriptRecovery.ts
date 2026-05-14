/**
 * useTranscriptRecovery Hook
 *
 * Orchestrates transcript recovery operations for interrupted meetings.
 * Provides functionality to detect, preview, and recover meetings from IndexedDB.
 */

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { indexedDBService, MeetingMetadata, StoredTranscript } from '@/services/indexedDBService';
import { storageService } from '@/services/storageService';

export interface UseTranscriptRecoveryReturn {
  recoverableMeetings: MeetingMetadata[];
  isLoading: boolean;
  isRecovering: boolean;
  checkForRecoverableTranscripts: () => Promise<void>;
  recoverMeeting: (meetingId: string) => Promise<{ success: boolean; audioRecoveryStatus?: null; meetingId?: string }>;
  loadMeetingTranscripts: (meetingId: string) => Promise<StoredTranscript[]>;
  deleteRecoverableMeeting: (meetingId: string) => Promise<void>;
}

export function useTranscriptRecovery(): UseTranscriptRecoveryReturn {
  const [recoverableMeetings, setRecoverableMeetings] = useState<MeetingMetadata[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [isRecovering, setIsRecovering] = useState(false);

  /**
   * Check for recoverable meetings in IndexedDB
   */
  const checkForRecoverableTranscripts = useCallback(async () => {
    setIsLoading(true);
    try {
      const meetings = await indexedDBService.getAllMeetings();

      // Filter out meetings older than 7 days and newer than 15 seconds
      const cutoffTime = Date.now() - (7 * 24 * 60 * 60 * 1000);
      const secondsAgo = Date.now() - (15 * 1000);

      const recentMeetings = meetings.filter(m => {
        const isWithinRetention = m.lastUpdated > cutoffTime;
        const isOldEnough = m.lastUpdated < secondsAgo;
        return isWithinRetention && isOldEnough;
      });

      setRecoverableMeetings(recentMeetings);
    } catch (error) {
      console.error('Failed to check for recoverable transcripts:', error);
      setRecoverableMeetings([]);
    } finally {
      setIsLoading(false);
    }
  }, []);

  /**
   * Load transcripts for preview
   */
  const loadMeetingTranscripts = useCallback(async (meetingId: string): Promise<StoredTranscript[]> => {
    try {
      const transcripts = await indexedDBService.getTranscripts(meetingId);
      transcripts.sort((a, b) => (a.sequenceId || 0) - (b.sequenceId || 0));
      return transcripts;
    } catch (error) {
      console.error('Failed to load meeting transcripts:', error);
      return [];
    }
  }, []);

  /**
   * Recover a meeting from IndexedDB
   */
  const recoverMeeting = useCallback(async (meetingId: string): Promise<{ success: boolean; audioRecoveryStatus?: null; meetingId?: string }> => {
    setIsRecovering(true);
    try {
      // 1. Load meeting metadata
      const metadata = await indexedDBService.getMeetingMetadata(meetingId);
      if (!metadata) {
        throw new Error('Meeting metadata not found');
      }

      // 2. Load all transcripts
      const transcripts = await loadMeetingTranscripts(meetingId);
      if (transcripts.length === 0) {
        throw new Error('No transcripts found for this meeting');
      }

      // 3. Check for folder path
      let folderPath = metadata.folderPath;

      if (!folderPath) {
        try {
          folderPath = await invoke<string>('get_meeting_folder_path');
        } catch {
          folderPath = undefined;
        }
      }

      // 4. Convert StoredTranscripts to the format expected by storageService
      const formattedTranscripts = transcripts.map((t, index) => ({
        id: t.id?.toString() || `${Date.now()}-${index}`,
        text: t.text,
        timestamp: t.timestamp,
        sequence_id: t.sequenceId || index,
        chunk_start_time: t['chunk_start_time'] as number | undefined,
        is_partial: (t['is_partial'] as boolean | undefined) || false,
        confidence: t.confidence,
        audio_start_time: t.audio_start_time,
        audio_end_time: t.audio_end_time,
        duration: t.duration,
      }));

      // 5. Save to backend database
      const saveResponse = await storageService.saveMeeting(
        metadata.title,
        formattedTranscripts,
        folderPath ?? null
      );

      const savedMeetingId = saveResponse.meeting_id;

      // 6. Mark as saved in IndexedDB
      await indexedDBService.markMeetingSaved(meetingId);

      // 7. Remove from recoverable list
      setRecoverableMeetings(prev => prev.filter(m => m.meetingId !== meetingId));

      return {
        success: true,
        audioRecoveryStatus: null,
        meetingId: savedMeetingId
      };
    } catch (error) {
      console.error('Failed to recover meeting:', error);
      throw error;
    } finally {
      setIsRecovering(false);
    }
  }, [loadMeetingTranscripts]);

  /**
   * Delete a recoverable meeting
   */
  const deleteRecoverableMeeting = useCallback(async (meetingId: string): Promise<void> => {
    try {
      await indexedDBService.deleteMeeting(meetingId);
      setRecoverableMeetings(prev => prev.filter(m => m.meetingId !== meetingId));
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
    deleteRecoverableMeeting
  };
}
