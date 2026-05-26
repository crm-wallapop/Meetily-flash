import { useEffect, useCallback, useRef } from 'react';
import { useRouter } from 'next/navigation';
import { toast } from 'sonner';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { useRecordingState, RecordingStatus } from '@/contexts/RecordingStateContext';
import { recordingService, type StopRecordingResult } from '@/services/recordingService';
import { enqueueTranscriptionJob } from '@/services/queueService';
import Analytics from '@/lib/analytics';

type SummaryStatus = 'idle' | 'processing' | 'summarizing' | 'regenerating' | 'completed' | 'error';

interface UseRecordingStopReturn {
  handleRecordingStop: (callApi: boolean) => Promise<void>;
  isStopping: boolean;
  isProcessingTranscript: boolean;
  isSavingTranscript: boolean;
  summaryStatus: SummaryStatus;
  setIsStopping: (value: boolean) => void;
}

/**
 * Custom hook for managing recording stop lifecycle.
 * SQLite save runs in Rust's background_shutdown; this hook enqueues transcription,
 * shows the toast, and listens for recording-saved-to-db to update the sidebar.
 */
export function useRecordingStop(
  setIsRecording: (value: boolean) => void,
  setIsRecordingDisabled: (value: boolean) => void
): UseRecordingStopReturn {
  const recordingState = useRecordingState();
  const {
    status,
    setStatus,
    isStopping,
    isProcessing: isProcessingTranscript,
    isSaving: isSavingTranscript
  } = recordingState;

  const {
    transcriptsRef,
    clearTranscripts,
    meetingTitle,
    markMeetingAsSaved,
    activeMeetingId,
    setActiveMeetingId,
  } = useTranscripts();

  const {
    refetchMeetings,
    setCurrentMeeting,
    setIsMeetingActive,
  } = useSidebar();

  const router = useRouter();
  const stopInProgressRef = useRef(false);
  const meetingTitleRef = useRef(meetingTitle);
  const activeMeetingIdRef = useRef(activeMeetingId);

  useEffect(() => {
    meetingTitleRef.current = meetingTitle;
    activeMeetingIdRef.current = activeMeetingId;
  }, [meetingTitle, activeMeetingId]);

  // Listen for recording-saved-to-db (SQLite row committed by Rust background_shutdown)
  useEffect(() => {
    let unlistenDb: (() => void) | undefined;
    let unlistenFailed: (() => void) | undefined;

    const setup = async () => {
      unlistenDb = await recordingService.onRecordingSavedToDb(async ({ meeting_id }) => {
        const expectedId = activeMeetingIdRef.current;
        if (expectedId && expectedId !== meeting_id) {
          console.log('Ignoring stale recording-saved-to-db for', meeting_id, '(expected', expectedId, ')');
          return;
        }
        console.log('✅ recording-saved-to-db:', meeting_id);
        await refetchMeetings();
        setCurrentMeeting({ id: meeting_id, title: meetingTitleRef.current || 'New Meeting' });
        await markMeetingAsSaved();
        setActiveMeetingId(null);
      });

      unlistenFailed = await recordingService.onRecordingSaveFailed((error) => {
        console.error('❌ recording-save-failed:', error);
        toast.error('Failed to save meeting to database', { description: error });
      });
    };

    setup();
    return () => {
      unlistenDb?.();
      unlistenFailed?.();
    };
  }, [refetchMeetings, setCurrentMeeting, markMeetingAsSaved, setActiveMeetingId]);

  // Main recording stop handler
  const handleRecordingStop = useCallback(async (isCallApi: boolean) => {
    if (stopInProgressRef.current) {
      return;
    }
    stopInProgressRef.current = true;

    setStatus(RecordingStatus.STOPPING);
    setIsRecording(false);
    setIsRecordingDisabled(true);
    const stopStartTime = Date.now();

    let stopResult: StopRecordingResult = { folder_path: null, meeting_name: null, meeting_id: null };
    try {
      stopResult = await recordingService.stopRecording();
      console.log('✅ stop_recording returned:', stopResult);
    } catch (error) {
      const errMsg = error instanceof Error ? error.message : String(error);
      if (errMsg.toLowerCase().includes('no recording in progress')) {
        console.log('Backend already stopped; continuing with empty result');
      } else {
        console.error('stop_recording invoke failed:', error);
        stopInProgressRef.current = false;
        setStatus(RecordingStatus.ERROR, errMsg);
        setIsRecordingDisabled(false);
        setIsMeetingActive(false);
        return;
      }
    }

    try {
      console.log('Post-stop processing...', {
        stop_initiated_at: new Date(stopStartTime).toISOString(),
        current_transcript_count: transcriptsRef.current.length,
        folder_path: stopResult.folder_path,
        meeting_name: stopResult.meeting_name,
        meeting_id: stopResult.meeting_id,
      });

      if (!stopResult.folder_path && !stopResult.meeting_name) {
        console.log('stop_recording returned empty result (backend was already idle/saving); skipping');
        setStatus(RecordingStatus.IDLE);
        setIsRecordingDisabled(false);
        setIsMeetingActive(false);
        return;
      }

      const meetingId = stopResult.meeting_id || activeMeetingId;
      const folderPath = stopResult.folder_path;

      if (isCallApi) {
        // Enqueue transcription job — the SQLite save runs in Rust's background_shutdown
        if (folderPath && meetingId) {
          const audioPath = folderPath.replace(/\\/g, '/') + '/audio.mp4';
          try {
            await enqueueTranscriptionJob(meetingId, audioPath);
            console.log('✅ Transcription job enqueued for', meetingId);
          } catch (enqueueError) {
            console.error('Failed to enqueue transcription job:', enqueueError);
            toast.error('Transcription could not be queued.', {
              description: String(enqueueError),
            });
          }
        } else {
          console.warn('Cannot enqueue transcription: folderPath or meetingId is null');
        }

        setStatus(RecordingStatus.COMPLETED);

        // Show success toast immediately — sidebar refreshes when recording-saved-to-db fires.
        toast.success('Recording saved successfully!', {
          description: transcriptsRef.current.length > 0
            ? `${transcriptsRef.current.length} transcript segments.`
            : 'Transcription queued — processing in background.',
          action: {
            label: 'View Meeting',
            onClick: () => {
              if (meetingId) {
                router.push(`/meeting-details?id=${meetingId}`);
                clearTranscripts();
                Analytics.trackButtonClick('view_meeting_from_toast', 'recording_complete');
              }
            }
          },
          duration: 10000,
        });

        setStatus(RecordingStatus.IDLE);

        // Track meeting completion analytics
        try {
          const freshTranscripts = transcriptsRef.current;
          let durationSeconds = 0;
          if (freshTranscripts.length > 0 && freshTranscripts[0].audio_start_time !== undefined) {
            const lastTranscript = freshTranscripts[freshTranscripts.length - 1];
            durationSeconds = lastTranscript.audio_end_time || lastTranscript.audio_start_time || 0;
          }

          const transcriptWordCount = freshTranscripts
            .map(t => t.text.split(/\s+/).length)
            .reduce((a, b) => a + b, 0);

          const wordsPerMinute = durationSeconds > 0 ? transcriptWordCount / (durationSeconds / 60) : 0;
          const meetingsToday = await Analytics.getMeetingsCountToday();

          if (meetingId) {
            await Analytics.trackMeetingCompleted(meetingId, {
              duration_seconds: durationSeconds,
              transcript_segments: freshTranscripts.length,
              transcript_word_count: transcriptWordCount,
              words_per_minute: wordsPerMinute,
              meetings_today: meetingsToday
            });
          }

          await Analytics.updateMeetingCount();

          const { Store } = await import('@tauri-apps/plugin-store');
          const store = await Store.load('analytics.json');
          const totalMeetings = await store.get<number>('total_meetings');

          if (totalMeetings === 1) {
            const daysSinceInstall = await Analytics.calculateDaysSince('first_launch_date');
            await Analytics.track('user_activated', {
              meetings_count: '1',
              days_since_install: daysSinceInstall?.toString() || 'null',
              first_meeting_duration_seconds: durationSeconds.toString()
            });
          }
        } catch (analyticsError) {
          console.error('Failed to track meeting completion analytics:', analyticsError);
        }
      } else {
        setStatus(RecordingStatus.IDLE);
      }

      setIsMeetingActive(false);
      setIsRecordingDisabled(false);
    } catch (error) {
      console.error('Error in handleRecordingStop:', error);
      setStatus(RecordingStatus.ERROR, error instanceof Error ? error.message : 'Unknown error');
      setIsRecordingDisabled(false);
    } finally {
      stopInProgressRef.current = false;
    }
  }, [
    setIsRecording,
    setIsRecordingDisabled,
    setStatus,
    transcriptsRef,
    clearTranscripts,
    setIsMeetingActive,
    router,
    activeMeetingId,
  ]);

  const handleRecordingStopRef = useRef(handleRecordingStop);
  useEffect(() => {
    handleRecordingStopRef.current = handleRecordingStop;
  });

  useEffect(() => {
    (window as Window & { handleRecordingStop?: (callApi?: boolean) => void }).handleRecordingStop = (callApi: boolean = true) => {
      handleRecordingStopRef.current(callApi);
    };

    return () => {
      delete (window as Window & { handleRecordingStop?: (callApi?: boolean) => void }).handleRecordingStop;
    };
  }, []);

  const summaryStatus: SummaryStatus = 'idle';

  return {
    handleRecordingStop,
    isStopping,
    isProcessingTranscript,
    isSavingTranscript,
    summaryStatus,
    setIsStopping: (value: boolean) => {
      setStatus(value ? RecordingStatus.STOPPING : RecordingStatus.IDLE);
    },
  };
}
