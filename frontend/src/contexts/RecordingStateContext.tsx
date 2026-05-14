'use client';

import React, { createContext, useContext, useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { recordingService, type RecordingPhase } from '@/services/recordingService';

/**
 * Recording state synchronized with backend
 * This context provides a single source of truth for recording state
 * that automatically syncs with the Rust backend, solving:
 * 1. Page refresh desync (backend recording but UI shows stopped)
 * 2. Pause state visibility across components
 * 3. Comprehensive state for future features (reconnection, etc.)
 */

// Recording lifecycle status enum
export enum RecordingStatus {
  IDLE = 'idle',                          // Not recording
  STARTING = 'starting',                  // Initiating recording
  RECORDING = 'recording',                // Active recording
  STOPPING = 'stopping',                  // Stop initiated, waiting for backend
  PROCESSING_TRANSCRIPTS = 'processing',  // Transcription completion wait
  SAVING = 'saving',                      // Saving to database
  COMPLETED = 'completed',                // Successfully saved
  ERROR = 'error'                         // Error occurred
}

interface RecordingState {
  isRecording: boolean;           // Is a recording session active
  isPaused: boolean;              // Is the recording paused
  isActive: boolean;              // Is actively recording (recording && !paused)
  recordingDuration: number | null;  // Total duration including pauses
  activeDuration: number | null;     // Active recording time (excluding pauses)

  // Lifecycle status
  status: RecordingStatus;
  statusMessage?: string;  // Optional message for current status
  /** Backend phase from recording-state-changed event */
  phase: RecordingPhase;
}

interface RecordingStateContextType extends RecordingState {
  setStatus: (status: RecordingStatus, message?: string) => void;

  // Computed helpers
  isStopping: boolean;
  isProcessing: boolean;
  /** True only when backend phase is Saving (background shutdown in progress) */
  isSaving: boolean;
}

const RecordingStateContext = createContext<RecordingStateContextType | null>(null);

export const useRecordingState = () => {
  const context = useContext(RecordingStateContext);
  if (!context) {
    throw new Error('useRecordingState must be used within a RecordingStateProvider');
  }
  return context;
};

export function RecordingStateProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<RecordingState>({
    isRecording: false,
    isPaused: false,
    isActive: false,
    recordingDuration: null,
    activeDuration: null,
    status: RecordingStatus.IDLE,
    statusMessage: undefined,
    phase: 'Idle',
  });

  const pollingIntervalRef = useRef<NodeJS.Timeout | null>(null);

  // NEW: Status setter with logging
  const setStatus = useCallback((status: RecordingStatus, message?: string) => {
    console.log(`[RecordingState] Status: ${state.status} → ${status}`, message || '');

    setState(prev => ({
      ...prev,
      status,
      statusMessage: message,
    }));
  }, [state.status, state.isRecording, state.isPaused]);

  /**
   * Sync recording state with backend
   * Called on mount (fixes refresh desync) and periodically while recording
   */
  const syncWithBackend = async () => {
    try {
      const backendState = await recordingService.getRecordingState();
      const phase = backendState.phase ?? 'Idle';

      setState(prev => ({
        ...prev,
        isRecording: phase === 'Recording',
        isPaused: backendState.is_paused,
        isActive: backendState.is_active,
        recordingDuration: backendState.recording_duration,
        activeDuration: backendState.active_duration,
        phase,
        status: phase === 'Recording'
          ? RecordingStatus.RECORDING
          : phase === 'Saving'
            ? RecordingStatus.SAVING
            : prev.status === RecordingStatus.RECORDING
              ? RecordingStatus.COMPLETED
              : prev.status,
      }));

      console.log('[RecordingStateContext] Synced with backend:', backendState);
    } catch (error) {
      console.error('[RecordingStateContext] Failed to sync with backend:', error);
    }
  };

  /**
   * Start polling backend state (called when recording starts)
   */
  const startPolling = () => {
    if (pollingIntervalRef.current) {
      clearInterval(pollingIntervalRef.current);
    }

    console.log('[RecordingStateContext] Starting state polling (500ms interval)');
    pollingIntervalRef.current = setInterval(syncWithBackend, 500);
  };

  /**
   * Stop polling backend state (called when recording stops)
   */
  const stopPolling = () => {
    if (pollingIntervalRef.current) {
      console.log('[RecordingStateContext] Stopping state polling');
      clearInterval(pollingIntervalRef.current);
      pollingIntervalRef.current = null;
    }
  };

  /**
   * Set up event listeners for backend state changes
   */
  useEffect(() => {
    console.log('[RecordingStateContext] Setting up event listeners');
    const unsubscribers: (() => void)[] = [];

    const setupListeners = async () => {
      try {
        // Recording started
        const unlistenStarted = await recordingService.onRecordingStarted(() => {
          console.log('[RecordingStateContext] Recording started event');
          setState(prev => ({
            ...prev,
            isRecording: true,
            isPaused: false,
            isActive: true,
            status: RecordingStatus.RECORDING,  // NEW: Set status to RECORDING
          }));
          startPolling();
        });
        unsubscribers.push(unlistenStarted);

        // recording-state-changed: authoritative phase transitions from backend
        const unlistenPhaseChanged = await recordingService.onRecordingStateChanged((payload) => {
          console.log('[RecordingStateContext] Phase changed:', payload.phase);
          const phase = payload.phase;

          setState(prev => {
            if (phase === 'Recording') {
              return {
                ...prev,
                phase,
                isRecording: true,
                status: RecordingStatus.RECORDING,
                statusMessage: undefined,
              };
            } else if (phase === 'Saving') {
              return {
                ...prev,
                phase,
                isRecording: false,
                isActive: false,
                status: RecordingStatus.SAVING,
                statusMessage: 'Saving…',
              };
            } else {
              // Idle — background shutdown complete; keep polling stopped
              return {
                ...prev,
                phase,
                isRecording: false,
                isPaused: false,
                isActive: false,
                status: RecordingStatus.COMPLETED,
                statusMessage: undefined,
                recordingDuration: null,
                activeDuration: null,
              };
            }
          });

          if (phase !== 'Recording') {
            stopPolling();
          }
        });
        unsubscribers.push(unlistenPhaseChanged);

        // Recording stopped — backwards-compat: fires when background shutdown finishes (Idle)
        const unlistenStopped = await recordingService.onRecordingStopped((payload) => {
          console.log('[RecordingStateContext] Recording stopped event (compat):', payload);
          // The phase-based listener above already handles the Idle transition.
          // This listener is kept for any callers that depend on recording-stopped
          // (e.g., TranscriptContext which listens for folder_path).
        });
        unsubscribers.push(unlistenStopped);

        // Recording paused
        const unlistenPaused = await recordingService.onRecordingPaused(() => {
          console.log('[RecordingStateContext] Recording paused event');
          setState(prev => ({
            ...prev,
            isPaused: true,
            isActive: false,
          }));
        });
        unsubscribers.push(unlistenPaused);

        // Recording resumed
        const unlistenResumed = await recordingService.onRecordingResumed(() => {
          console.log('[RecordingStateContext] Recording resumed event');
          setState(prev => ({
            ...prev,
            isPaused: false,
            isActive: true,
          }));
        });
        unsubscribers.push(unlistenResumed);

        console.log('[RecordingStateContext] Event listeners set up successfully');
      } catch (error) {
        console.error('[RecordingStateContext] Failed to set up event listeners:', error);
      }
    };

    setupListeners();

    return () => {
      console.log('[RecordingStateContext] Cleaning up event listeners');
      unsubscribers.forEach(unsub => unsub());
      stopPolling();
    };
  }, []);

  /**
   * Initial sync on mount - CRITICAL for fixing refresh desync bug
   * If backend is recording but UI state is false, this will correct it
   */
  useEffect(() => {
    console.log('[RecordingStateContext] Initial mount - syncing with backend');
    syncWithBackend();
  }, []);

  // Computed helpers — phase is the authoritative source for isRecording/isSaving
  const contextValue = useMemo(() => ({
    ...state,
    setStatus,
    isRecording: state.phase === 'Recording',
    isSaving: state.phase === 'Saving',
    isStopping: state.status === RecordingStatus.STOPPING,
    isProcessing: state.status === RecordingStatus.PROCESSING_TRANSCRIPTS,
  }), [state, setStatus]);

  return (
    <RecordingStateContext.Provider value={contextValue}>
      {children}
    </RecordingStateContext.Provider>
  );
}
