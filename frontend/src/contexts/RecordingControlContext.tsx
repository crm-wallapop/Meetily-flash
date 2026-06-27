'use client';

import React, { createContext, useContext, useMemo, useState } from 'react';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { useRecordingStateSync } from '@/hooks/useRecordingStateSync';
import { useRecordingStart } from '@/hooks/useRecordingStart';
import { useRecordingStop } from '@/hooks/useRecordingStop';
import { useAutoDetect } from '@/hooks/useAutoDetect';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { AutoDetectBanner } from '@/components/AutoDetectBanner';

interface RecordingControlContextValue {
  isRecording: boolean;
  isRecordingDisabled: boolean;
  handleRecordingStart: (overrideTitle?: string, detectorStarted?: boolean) => Promise<void>;
  handleRecordingStop: (callApi: boolean) => Promise<void>;
  setIsStopping: (value: boolean) => void;
}

const RecordingControlContext = createContext<RecordingControlContextValue | null>(null);

export function useRecordingControl(): RecordingControlContextValue {
  const ctx = useContext(RecordingControlContext);
  if (!ctx) {
    throw new Error('useRecordingControl must be used within RecordingControlProvider');
  }
  return ctx;
}

// Hoisted from app/page.tsx so the meeting-detected / meeting-ended Tauri
// listeners survive navigation. Previously they lived in the home page and
// unmounted on route change (e.g. to /meeting-details), silently dropping
// detection events. The optimistic isRecording pair moves here unchanged —
// RecordingStateContext remains the authoritative backend-derived state.
export function RecordingControlProvider({ children }: { children: React.ReactNode }) {
  const [isRecording, setIsRecording] = useState(false);
  const { setIsMeetingActive } = useSidebar();
  const recordingState = useRecordingState();

  const { isRecordingDisabled, setIsRecordingDisabled } = useRecordingStateSync(
    isRecording,
    setIsRecording,
    setIsMeetingActive
  );
  const { handleRecordingStart } = useRecordingStart(isRecording, setIsRecording);
  const { handleRecordingStop, setIsStopping } = useRecordingStop(
    setIsRecording,
    setIsRecordingDisabled
  );

  const {
    banner: autoDetectBanner,
    detectTimeoutSeconds,
    stopTimeoutSeconds,
    handleBannerConfirm,
    handleBannerCancel,
  } = useAutoDetect({
    isRecording: recordingState.isRecording,
    handleRecordingStart,
    handleRecordingStop,
    setIsRecording,
  });

  const value = useMemo<RecordingControlContextValue>(
    () => ({ isRecording, isRecordingDisabled, handleRecordingStart, handleRecordingStop, setIsStopping }),
    [isRecording, isRecordingDisabled, handleRecordingStart, handleRecordingStop, setIsStopping]
  );

  return (
    <RecordingControlContext.Provider value={value}>
      {children}
      {autoDetectBanner.visible && (
        <AutoDetectBanner
          mode={autoDetectBanner.mode}
          initialTitle={autoDetectBanner.initialTitle}
          candidateTitles={autoDetectBanner.candidateTitles}
          onConfirm={handleBannerConfirm}
          onCancel={handleBannerCancel}
          timeoutSeconds={
            autoDetectBanner.mode === 'detect-prompt' ? detectTimeoutSeconds : stopTimeoutSeconds
          }
        />
      )}
    </RecordingControlContext.Provider>
  );
}
