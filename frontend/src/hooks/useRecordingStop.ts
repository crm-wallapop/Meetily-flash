import { useState, useEffect, useCallback, useRef } from 'react';
import { useRouter } from 'next/navigation';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { useRecordingState, RecordingStatus } from '@/contexts/RecordingStateContext';
import { storageService } from '@/services/storageService';
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
 * Handles the stop sequence: SQLite save → transcription job enqueue → navigation.
 */
export function useRecordingStop(
  setIsRecording: (value: boolean) => void,
  setIsRecordingDisabled: (value: boolean) => void
): UseRecordingStopReturn {
  // USE global state instead
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
  } = useTranscripts();

  const {
    refetchMeetings,
    setCurrentMeeting,
    setIsMeetingActive,
  } = useSidebar();

  const router = useRouter();

  // Guard to prevent duplicate/concurrent stop calls (e.g., from UI and tray simultaneously)
  const stopInProgressRef = useRef(false);

  // Set up recording-stopped listener as a fallback sessionStorage sink.
  // stop_recording returns folder_path/meeting_name synchronously (primary source), but a
  // future tray-driven stop that bypasses invoke() can still populate sessionStorage here.
  useEffect(() => {
    let unlistenFn: (() => void) | undefined;

    const setupRecordingStoppedListener = async () => {
      try {
        console.log('Setting up recording-stopped listener for navigation...');
        unlistenFn = await listen<{
          message: string;
          folder_path?: string;
          meeting_name?: string;
        }>('recording-stopped', (event) => {
          const { folder_path, meeting_name } = event.payload;
          if (folder_path) {
            sessionStorage.setItem('last_recording_folder_path', folder_path);
          }
          if (meeting_name) {
            sessionStorage.setItem('last_recording_meeting_name', meeting_name);
          }
        });
        console.log('Recording stopped listener setup complete');
      } catch (error) {
        console.error('Failed to setup recording stopped listener:', error);
      }
    };

    setupRecordingStoppedListener();

    return () => {
      console.log('Cleaning up recording stopped listener...');
      if (unlistenFn) {
        unlistenFn();
      }
    };
  }, []);

  // Main recording stop handler
  const handleRecordingStop = useCallback(async (isCallApi: boolean) => {
    // Guard: prevent duplicate/concurrent stop calls
    if (stopInProgressRef.current) {
      return;
    }
    stopInProgressRef.current = true;

    // onStopInitiated (button path) already called setStatus(STOPPING) synchronously.
    // Calling it again here is a no-op in React (same-value setState is deduplicated).
    // This ensures tray-driven stops (which skip the button) also enter STOPPING state.
    setStatus(RecordingStatus.STOPPING);
    setIsRecording(false);
    setIsRecordingDisabled(true);
    const stopStartTime = Date.now();

    // Invoke the backend stop here — this is the single call site for stop_recording.
    // Both the manual Stop button (RecordingControls) and the auto-detect banner (useAutoDetect)
    // route through this function, so the backend stop is guaranteed exactly once per stop.
    // stop_recording returns folder_path/meeting_name synchronously so we don't need to
    // race the recording-stopped event for the save flow.
    let stopResult: StopRecordingResult = { folder_path: null, meeting_name: null };
    try {
      stopResult = await recordingService.stopRecording();
      console.log('✅ stop_recording returned:', stopResult);
    } catch (error) {
      const errMsg = error instanceof Error ? error.message : String(error);
      // "No recording in progress" is benign — the backend was already idle.
      if (errMsg.toLowerCase().includes('no recording in progress')) {
        console.log('Backend already stopped; continuing save flow with empty result');
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
      });

      // If the backend returned an empty result without an error, it was already Idle or
      // mid-Saving for another stop. Don't save a shell DB row — just reset state.
      if (!stopResult.folder_path && !stopResult.meeting_name) {
        console.log('stop_recording returned empty result (backend was already idle/saving); skipping save');
        setStatus(RecordingStatus.IDLE);
        setIsRecordingDisabled(false);
        setIsMeetingActive(false);
        return;
      }

      // Save to SQLite
      // NOTE: enabled to save COMPLETE transcripts after frontend receives all updates
      // This ensures user sees all transcripts streaming in before database save
      if (isCallApi) {

        setStatus(RecordingStatus.SAVING, 'Saving meeting to database...');

        // Get fresh transcript state (ALL transcripts including late ones)
        const freshTranscripts = [...transcriptsRef.current];

        // folder_path/meeting_name came directly from stop_recording's return value.
        // Fall back to the event-driven sessionStorage in case something else routes
        // through here without going through invoke (e.g., a future tray-driven stop).
        const folderPath =
          stopResult.folder_path ?? sessionStorage.getItem('last_recording_folder_path');
        const savedMeetingName =
          stopResult.meeting_name ?? sessionStorage.getItem('last_recording_meeting_name');

        // Re-enable recording button before the HTTP save.
        // stop_recording has returned (phase → Saving, manager ownership handed off to
        // background_shutdown), but WASAPI stream teardown is still running in the background.
        // M2 starting here will race M1's teardown on the same audio endpoints — this is
        // intentional and safe: cpal handles concurrent open/close on Windows loopback, and
        // the transcript snapshot (freshTranscripts) was already taken so M2 traffic can't
        // contaminate M1's save.
        setIsRecordingDisabled(false);

        console.log('💾 Saving COMPLETE transcripts to database...', {
          transcript_count: freshTranscripts.length,
          meeting_name: savedMeetingName || meetingTitle,
          folder_path: folderPath,
          sample_text: freshTranscripts.length > 0 ? freshTranscripts[0].text.substring(0, 50) + '...' : 'none',
          last_transcript: freshTranscripts.length > 0 ? freshTranscripts[freshTranscripts.length - 1].text.substring(0, 30) + '...' : 'none',
        });

        try {
          const responseData = await storageService.saveMeeting(
            savedMeetingName || meetingTitle || 'New Meeting',  // PREFER savedMeetingName (backend source)
            freshTranscripts,
            folderPath
          );

          const meetingId = responseData.meeting_id;
          if (!meetingId) {
            console.error('No meeting_id in response:', responseData);
            throw new Error('No meeting ID received from save operation');
          }

          console.log('✅ Successfully saved COMPLETE meeting with ID:', meetingId);
          console.log('   Transcripts:', freshTranscripts.length);
          console.log('   folder_path:', folderPath);

          // Enqueue transcription job using the UUID from the DB row so the queue
          // and the meeting view agree on the meeting_id.
          if (folderPath) {
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
            console.warn('Cannot enqueue transcription: folderPath is null for meetingId', meetingId);
            toast.error('Transcription could not be queued — audio path is unknown.');
          }

          // Mark meeting as saved in IndexedDB (for recovery system)
          await markMeetingAsSaved();

          // Clean up session storage
          sessionStorage.removeItem('last_recording_folder_path');
          sessionStorage.removeItem('last_recording_meeting_name');
          // Clean up IndexedDB meeting ID (redundant with markMeetingAsSaved cleanup, but ensures cleanup)
          sessionStorage.removeItem('indexeddb_current_meeting_id');

          // Refetch meetings and set current meeting
          await refetchMeetings();

          try {
            const meetingData = await storageService.getMeeting(meetingId);
            if (meetingData) {
              setCurrentMeeting({
                id: meetingId,
                title: meetingData.title
              });
              console.log('✅ Current meeting set:', meetingData.title);
            }
          } catch (error) {
            console.warn('Could not fetch meeting details, using ID only:', error);
            setCurrentMeeting({ id: meetingId, title: savedMeetingName || meetingTitle || 'New Meeting' });
          }

          // Mark as completed
          setStatus(RecordingStatus.COMPLETED);

          // Show success toast at the top so it doesn't overlap the recording button.
          // Auto-navigate is intentionally absent: the user may want to start M2
          // immediately. "View Meeting" is the voluntary path to the details page.
          toast.success('Recording saved successfully!', {
            description: freshTranscripts.length > 0
              ? `${freshTranscripts.length} transcript segments saved.`
              : 'Transcription queued — processing in background.',
            action: {
              label: 'View Meeting',
              onClick: () => {
                router.push(`/meeting-details?id=${meetingId}`);
                clearTranscripts();
                Analytics.trackButtonClick('view_meeting_from_toast', 'recording_complete');
              }
            },
            duration: 10000,
          });

          setStatus(RecordingStatus.IDLE);
          // Track meeting completion analytics
          try {
            // Calculate meeting duration from transcript timestamps
            let durationSeconds = 0;
            if (freshTranscripts.length > 0 && freshTranscripts[0].audio_start_time !== undefined) {
              // Use audio_end_time of last transcript if available
              const lastTranscript = freshTranscripts[freshTranscripts.length - 1];
              durationSeconds = lastTranscript.audio_end_time || lastTranscript.audio_start_time || 0;
            }

            // Calculate word count
            const transcriptWordCount = freshTranscripts
              .map(t => t.text.split(/\s+/).length)
              .reduce((a, b) => a + b, 0);

            // Calculate words per minute
            const wordsPerMinute = durationSeconds > 0 ? transcriptWordCount / (durationSeconds / 60) : 0;

            // Get meetings count today
            const meetingsToday = await Analytics.getMeetingsCountToday();

            // Track meeting completed
            await Analytics.trackMeetingCompleted(meetingId, {
              duration_seconds: durationSeconds,
              transcript_segments: freshTranscripts.length,
              transcript_word_count: transcriptWordCount,
              words_per_minute: wordsPerMinute,
              meetings_today: meetingsToday
            });

            // Update meeting count in analytics.json
            await Analytics.updateMeetingCount();

            // Check for activation (first meeting)
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
            // Don't block user flow on analytics errors
          }

        } catch (saveError) {
          console.error('Failed to save meeting to database:', saveError);
          setStatus(RecordingStatus.ERROR, saveError instanceof Error ? saveError.message : 'Unknown error');
          toast.error('Failed to save meeting', {
            description: saveError instanceof Error ? saveError.message : 'Unknown error'
          });
          throw saveError;
        }
      } else {
        // No save needed, go back to IDLE
        setStatus(RecordingStatus.IDLE);
      }

      setIsMeetingActive(false);
      // isRecording already set to false at function start
      setIsRecordingDisabled(false);
    } catch (error) {
      console.error('Error in handleRecordingStop:', error);
      setStatus(RecordingStatus.ERROR, error instanceof Error ? error.message : 'Unknown error');
      // isRecording already set to false at function start
      setIsRecordingDisabled(false);
    } finally {
      // Always reset the guard flag when done
      stopInProgressRef.current = false;
    }
  }, [
    setIsRecording,
    setIsRecordingDisabled,
    setStatus,
    transcriptsRef,
    clearTranscripts,
    meetingTitle,
    markMeetingAsSaved,
    refetchMeetings,
    setCurrentMeeting,
    setIsMeetingActive,
    router,
  ]);

  // Expose handleRecordingStop function to window for Rust callbacks
  const handleRecordingStopRef = useRef(handleRecordingStop);
  useEffect(() => {
    handleRecordingStopRef.current = handleRecordingStop;
  });

  useEffect(() => {
    (window as Window & { handleRecordingStop?: (callApi?: boolean) => void }).handleRecordingStop = (callApi: boolean = true) => {
      handleRecordingStopRef.current(callApi);
    };

    // Cleanup on unmount
    return () => {
      delete (window as Window & { handleRecordingStop?: (callApi?: boolean) => void }).handleRecordingStop;
    };
  }, []);

  // PROCESSING_TRANSCRIPTS is never set in the post-meeting-transcription flow;
  // transcription runs asynchronously in the queue.
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
