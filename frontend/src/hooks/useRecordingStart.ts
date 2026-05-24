import { useState, useEffect, useCallback } from 'react';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { useConfig } from '@/contexts/ConfigContext';
import { useRecordingState, RecordingStatus } from '@/contexts/RecordingStateContext';
import { recordingService } from '@/services/recordingService';
import Analytics from '@/lib/analytics';
import { showRecordingNotification } from '@/lib/recordingNotification';

interface UseRecordingStartReturn {
  handleRecordingStart: (overrideTitle?: string) => Promise<void>;
  isAutoStarting: boolean;
}

export function useRecordingStart(
  isRecording: boolean,
  setIsRecording: (value: boolean) => void,
  showModal?: (name: 'modelSelector', message?: string) => void
): UseRecordingStartReturn {
  const [isAutoStarting, setIsAutoStarting] = useState(false);

  const { clearTranscripts, setMeetingTitle, setActiveMeetingId } = useTranscripts();
  const { setIsMeetingActive } = useSidebar();
  const { selectedDevices } = useConfig();
  const { setStatus } = useRecordingState();

  const generateMeetingTitle = useCallback(() => {
    const now = new Date();
    const day = String(now.getDate()).padStart(2, '0');
    const month = String(now.getMonth() + 1).padStart(2, '0');
    const year = String(now.getFullYear()).slice(-2);
    const hours = String(now.getHours()).padStart(2, '0');
    const minutes = String(now.getMinutes()).padStart(2, '0');
    const seconds = String(now.getSeconds()).padStart(2, '0');
    return `Meeting ${day}_${month}_${year}_${hours}_${minutes}_${seconds}`;
  }, []);

  const handleRecordingStart = useCallback(async (overrideTitle?: string) => {
    try {
      const title = overrideTitle || generateMeetingTitle();
      setMeetingTitle(title);
      setStatus(RecordingStatus.STARTING, 'Initializing recording...');

      const result = await recordingService.startRecordingWithDevices(
        selectedDevices?.micDevice || null,
        selectedDevices?.systemDevice || null,
        title
      );
      setActiveMeetingId(result.meeting_id);

      setIsRecording(true);
      clearTranscripts();
      setIsMeetingActive(true);
      Analytics.trackButtonClick('start_recording', 'home_page');
      await showRecordingNotification();
    } catch (error) {
      console.error('Failed to start recording:', error);
      setStatus(RecordingStatus.ERROR, error instanceof Error ? error.message : 'Failed to start recording');
      setIsRecording(false);
      Analytics.trackButtonClick('start_recording_error', 'home_page');
      throw error;
    }
  }, [generateMeetingTitle, setMeetingTitle, setIsRecording, clearTranscripts, setIsMeetingActive, selectedDevices, setStatus, setActiveMeetingId]);

  // Auto-start from sidebar navigation (sessionStorage flag)
  useEffect(() => {
    const checkAutoStart = async () => {
      if (typeof window === 'undefined') return;
      const shouldAutoStart = sessionStorage.getItem('autoStartRecording');
      if (shouldAutoStart !== 'true' || isRecording || isAutoStarting) return;

      setIsAutoStarting(true);
      sessionStorage.removeItem('autoStartRecording');

      try {
        const title = generateMeetingTitle();
        setStatus(RecordingStatus.STARTING, 'Initializing recording...');
        await recordingService.startRecordingWithDevices(
          selectedDevices?.micDevice || null,
          selectedDevices?.systemDevice || null,
          title
        ).then(r => { setActiveMeetingId(r.meeting_id); return r; });
        setMeetingTitle(title);
        setIsRecording(true);
        clearTranscripts();
        setIsMeetingActive(true);
        Analytics.trackButtonClick('start_recording', 'sidebar_auto');
        await showRecordingNotification();
      } catch (error) {
        console.error('Failed to auto-start recording:', error);
        setStatus(RecordingStatus.ERROR, error instanceof Error ? error.message : 'Failed to auto-start recording');
        Analytics.trackButtonClick('start_recording_error', 'sidebar_auto');
      } finally {
        setIsAutoStarting(false);
      }
    };

    checkAutoStart();
  }, [isRecording, isAutoStarting, selectedDevices, generateMeetingTitle, setMeetingTitle, setIsRecording, clearTranscripts, setIsMeetingActive, setStatus, setActiveMeetingId]);

  // Direct trigger from sidebar when already on the home page
  useEffect(() => {
    const handleDirectStart = async () => {
      if (isRecording || isAutoStarting) return;

      setIsAutoStarting(true);
      try {
        const title = generateMeetingTitle();
        setStatus(RecordingStatus.STARTING, 'Initializing recording...');
        await recordingService.startRecordingWithDevices(
          selectedDevices?.micDevice || null,
          selectedDevices?.systemDevice || null,
          title
        ).then(r => { setActiveMeetingId(r.meeting_id); return r; });
        setMeetingTitle(title);
        setIsRecording(true);
        clearTranscripts();
        setIsMeetingActive(true);
        Analytics.trackButtonClick('start_recording', 'sidebar_direct');
        await showRecordingNotification();
      } catch (error) {
        console.error('Failed to start recording from sidebar:', error);
        setStatus(RecordingStatus.ERROR, error instanceof Error ? error.message : 'Failed to start recording from sidebar');
        Analytics.trackButtonClick('start_recording_error', 'sidebar_direct');
      } finally {
        setIsAutoStarting(false);
      }
    };

    window.addEventListener('start-recording-from-sidebar', handleDirectStart);
    return () => window.removeEventListener('start-recording-from-sidebar', handleDirectStart);
  }, [isRecording, isAutoStarting, selectedDevices, generateMeetingTitle, setMeetingTitle, setIsRecording, clearTranscripts, setIsMeetingActive, setStatus, setActiveMeetingId]);

  return { handleRecordingStart, isAutoStarting };
}
