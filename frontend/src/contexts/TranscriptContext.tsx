'use client';

import React, { createContext, useContext, useState, useEffect, useRef, useCallback, ReactNode, MutableRefObject } from 'react';
import { Transcript } from '@/types';
import { toast } from 'sonner';
import { recordingService } from '@/services/recordingService';
import { indexedDBService } from '@/services/indexedDBService';

interface TranscriptContextType {
  transcripts: Transcript[];
  transcriptsRef: MutableRefObject<Transcript[]>
  copyTranscript: () => void;
  transcriptContainerRef: React.RefObject<HTMLDivElement>;
  meetingTitle: string;
  setMeetingTitle: (title: string) => void;
  clearTranscripts: () => void;
  currentMeetingId: string | null;
  markMeetingAsSaved: () => Promise<void>;
  activeMeetingId: string | null;
  setActiveMeetingId: (id: string | null) => void;
}

const TranscriptContext = createContext<TranscriptContextType | undefined>(undefined);

export interface RecordingStartedIds {
  meetingId: string;
  activeMeetingId: string | null;
}

export function deriveRecordingStartedIds(
  payload: Record<string, unknown> | undefined,
  now: number
): RecordingStartedIds {
  const rustMeetingId = payload?.meeting_id as string | undefined;
  return {
    meetingId: rustMeetingId || `meeting-${now}`,
    activeMeetingId: rustMeetingId || null,
  };
}

export function recordingStartedFallbackTitle(now: number): string {
  const isoStamp = new Date(now).toISOString().slice(0, 19).replace('T', '_').replace(/:/g, '-');
  return `Meeting ${isoStamp}`;
}

export function TranscriptProvider({ children }: { children: ReactNode }) {
  const [transcripts, setTranscripts] = useState<Transcript[]>([]);
  const [meetingTitle, setMeetingTitle] = useState('+ New Call');
  const [currentMeetingId, setCurrentMeetingId] = useState<string | null>(null);
  const [activeMeetingId, setActiveMeetingId] = useState<string | null>(null);

  // Refs for transcript management
  const transcriptsRef = useRef<Transcript[]>(transcripts);
  const isUserAtBottomRef = useRef<boolean>(true);
  const transcriptContainerRef = useRef<HTMLDivElement>(null);

  // Keep ref updated with current transcripts
  useEffect(() => {
    transcriptsRef.current = transcripts;
  }, [transcripts]);

  // Smart auto-scroll: Track user scroll position
  useEffect(() => {
    const handleScroll = () => {
      const container = transcriptContainerRef.current;
      if (!container) return;

      const { scrollTop, scrollHeight, clientHeight } = container;
      const isAtBottom = scrollTop + clientHeight >= scrollHeight - 10; // 10px tolerance
      isUserAtBottomRef.current = isAtBottom;
    };

    const container = transcriptContainerRef.current;
    if (container) {
      container.addEventListener('scroll', handleScroll);
      return () => container.removeEventListener('scroll', handleScroll);
    }
  }, []);

  // Auto-scroll when transcripts change (only if user is at bottom)
  useEffect(() => {
    // Only auto-scroll if user was at the bottom before new content
    if (isUserAtBottomRef.current && transcriptContainerRef.current) {
      // Wait for Framer Motion animation to complete (150ms) before scrolling
      // This ensures scrollHeight includes the full rendered height of the new transcript
      const scrollTimeout = setTimeout(() => {
        const container = transcriptContainerRef.current;
        if (container) {
          container.scrollTo({
            top: container.scrollHeight,
            behavior: 'smooth'
          });
        }
      }, 150); // Match Framer Motion transition duration

      return () => clearTimeout(scrollTimeout);
    }
  }, [transcripts]);

  // Initialize IndexedDB and listen for recording-started/stopped events
  useEffect(() => {
    let unlistenRecordingStarted: (() => void) | undefined;

    const setupRecordingListeners = async () => {
      try {
        // Initialize IndexedDB (opens v2 schema with transcription_queue store).
        await indexedDBService.init();

        // Listen for recording-started event
        unlistenRecordingStarted = await recordingService.onRecordingStarted(async (event) => {
          try {
            const payload = event.payload as Record<string, unknown> | undefined;
            const ids = deriveRecordingStartedIds(payload, Date.now());
            setCurrentMeetingId(ids.meetingId);
            setActiveMeetingId(ids.activeMeetingId);
            sessionStorage.setItem('indexeddb_current_meeting_id', ids.meetingId);

            const meetingName = await recordingService.getRecordingMeetingName();
            setMeetingTitle(meetingName || recordingStartedFallbackTitle(Date.now()));
          } catch (error) {
            console.error('Failed to initialize meeting on recording start:', error);
          }
        });
      } catch (error) {
        console.error('Failed to setup recording listeners:', error);
      }
    };

    setupRecordingListeners();

    return () => {
      if (unlistenRecordingStarted) {
        unlistenRecordingStarted();
      }
    };
  }, [currentMeetingId]);

  // Copy transcript to clipboard with recording-relative timestamps
  const copyTranscript = useCallback(() => {
    // Format timestamps as recording-relative [MM:SS] instead of wall-clock time
    const formatTime = (seconds: number | undefined): string => {
      if (seconds === undefined) return '[--:--]';
      const totalSecs = Math.floor(seconds);
      const mins = Math.floor(totalSecs / 60);
      const secs = totalSecs % 60;
      return `[${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}]`;
    };

    const fullTranscript = transcripts
      .map(t => `${formatTime(t.audio_start_time)} ${t.text}`)
      .join('\n');
    navigator.clipboard.writeText(fullTranscript);

    toast.success("Transcript copied to clipboard");
  }, [transcripts]);

  // Clear transcripts (used when starting new recording)
  const clearTranscripts = useCallback(() => {
    setTranscripts([]);
    // Don't clear currentMeetingId here - it will be set by recording-started event
  }, []);

  // Mark current meeting as saved in IndexedDB
  const markMeetingAsSaved = useCallback(async () => {
    // Try context state first, fallback to sessionStorage
    const meetingId = currentMeetingId || sessionStorage.getItem('indexeddb_current_meeting_id');

    if (!meetingId) {
      console.error('[IndexedDB] ❌ Cannot mark meeting as saved: No meeting ID available!');
      console.error('[IndexedDB] currentMeetingId:', currentMeetingId);
      console.error('[IndexedDB] sessionStorage:', sessionStorage.getItem('indexeddb_current_meeting_id'));
      return;
    }

    try {
      await indexedDBService.markMeetingSaved(meetingId);

      // Clear both sources
      setCurrentMeetingId(null);
      sessionStorage.removeItem('indexeddb_current_meeting_id');
    } catch (error) {
      console.error('[IndexedDB] ❌ Failed to mark meeting as saved:', error);
    }
  }, [currentMeetingId]);

  const value: TranscriptContextType = {
    transcripts,
    transcriptsRef,
    copyTranscript,
    transcriptContainerRef,
    meetingTitle,
    setMeetingTitle,
    clearTranscripts,
    currentMeetingId,
    markMeetingAsSaved,
    activeMeetingId,
    setActiveMeetingId,
  };

  return (
    <TranscriptContext.Provider value={value}>
      {children}
    </TranscriptContext.Provider>
  );
}

export function useTranscripts() {
  const context = useContext(TranscriptContext);
  if (context === undefined) {
    throw new Error('useTranscripts must be used within a TranscriptProvider');
  }
  return context;
}
