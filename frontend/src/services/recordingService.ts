/**
 * Recording Service
 *
 * Handles all recording lifecycle Tauri backend calls and events.
 * Pure 1-to-1 wrapper - no error handling changes, exact same behavior as direct invoke/listen calls.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';

export type RecordingPhase = 'Idle' | 'Recording' | 'Saving';

export interface RecordingState {
  is_recording: boolean;
  is_paused: boolean;
  is_active: boolean;
  recording_duration: number | null;
  active_duration: number | null;
  /** Phase reflects the three-state lifecycle: Idle | Recording | Saving */
  phase: RecordingPhase;
}

export interface RecordingStoppedPayload {
  message: string;
  folder_path?: string;
  meeting_name?: string;
}

/**
 * Result returned synchronously from stop_recording.
 * Use these values directly for the save flow — they're guaranteed to be
 * populated before invoke() resolves, unlike the recording-stopped event
 * which has Tauri-event-delivery timing semantics.
 */
export interface StopRecordingResult {
  folder_path: string | null;
  meeting_name: string | null;
}

/**
 * Payload of `recording-saved` event, fired when the MP4 finalize step completes
 * inside the background shutdown task. Use for UI refresh of audio players,
 * NOT for the meeting-row save (use the return value of stop_recording for that).
 */
export interface RecordingSavedPayload {
  audio_file: string;
  transcript_file?: string;
  meeting_name?: string;
  meeting_folder?: string;
}

export interface RecordingStateChangedPayload {
  phase: RecordingPhase;
}

/**
 * Recording Service
 * Singleton service for managing recording lifecycle operations
 */
export class RecordingService {
  /**
   * Check if recording is currently active
   * @returns Promise<boolean>
   */
  async isRecording(): Promise<boolean> {
    return invoke<boolean>('is_recording');
  }

  /**
   * Get comprehensive recording state (includes durations)
   * @returns Promise with full recording state
   */
  async getRecordingState(): Promise<RecordingState> {
    return invoke<RecordingState>('get_recording_state');
  }

  /**
   * Get current meeting name
   * @returns Promise<string | null>
   */
  async getRecordingMeetingName(): Promise<string | null> {
    return invoke<string | null>('get_recording_meeting_name');
  }

  /**
   * Start recording (no device configuration)
   * @returns Promise<void>
   */
  async startRecording(): Promise<void> {
    return invoke('start_recording');
  }

  /**
   * Start recording with device configuration and meeting name
   * @param micDeviceName - Microphone device name (null for default)
   * @param systemDeviceName - System audio device name (null for none)
   * @param meetingName - Meeting name/title
   * @returns Promise<void>
   */
  async startRecordingWithDevices(
    micDeviceName: string | null,
    systemDeviceName: string | null,
    meetingName: string
  ): Promise<void> {
    return invoke('start_recording_with_devices_and_meeting', {
      mic_device_name: micDeviceName,
      system_device_name: systemDeviceName,
      meeting_name: meetingName
    });
  }

  /**
   * Stop the active recording. Returns synchronously with folder/meeting info so the
   * save flow can write a DB row with folder_path populated. The audio file itself is
   * finalized asynchronously and announced via the `recording-saved` event.
   */
  async stopRecording(): Promise<StopRecordingResult> {
    return invoke<StopRecordingResult>('stop_recording');
  }

  /**
   * Pause active recording
   * @returns Promise<void>
   */
  async pauseRecording(): Promise<void> {
    return invoke('pause_recording');
  }

  /**
   * Resume paused recording
   * @returns Promise<void>
   */
  async resumeRecording(): Promise<void> {
    return invoke('resume_recording');
  }

  // Event Listeners

  /**
   * Listen for recording-started event
   * @param callback - Function to call when recording starts
   * @returns Promise that resolves to unlisten function
   */
  async onRecordingStarted(callback: () => void): Promise<UnlistenFn> {
    return listen('recording-started', callback);
  }

  /**
   * Listen for recording-stopped event (with metadata)
   * @param callback - Function to call when recording stops
   * @returns Promise that resolves to unlisten function
   */
  async onRecordingStopped(callback: (payload: RecordingStoppedPayload) => void): Promise<UnlistenFn> {
    return listen<RecordingStoppedPayload>('recording-stopped', (event) => {
      callback(event.payload);
    });
  }

  /**
   * Listen for recording-paused event
   * @param callback - Function to call when recording is paused
   * @returns Promise that resolves to unlisten function
   */
  async onRecordingPaused(callback: () => void): Promise<UnlistenFn> {
    return listen('recording-paused', callback);
  }

  /**
   * Listen for recording-resumed event
   * @param callback - Function to call when recording resumes
   * @returns Promise that resolves to unlisten function
   */
  async onRecordingResumed(callback: () => void): Promise<UnlistenFn> {
    return listen('recording-resumed', callback);
  }

  /**
   * Listen for recording-state-changed event (phase transitions: Idle | Recording | Saving)
   * @param callback - Function to call on every phase transition
   * @returns Promise that resolves to unlisten function
   */
  async onRecordingStateChanged(
    callback: (payload: RecordingStateChangedPayload) => void
  ): Promise<UnlistenFn> {
    return listen<RecordingStateChangedPayload>('recording-state-changed', (event) => {
      callback(event.payload);
    });
  }

  /**
   * Listen for recording-save-failed event (background shutdown error)
   * @param callback - Function to call when background save fails
   * @returns Promise that resolves to unlisten function
   */
  async onRecordingSaveFailed(callback: (error: string) => void): Promise<UnlistenFn> {
    return listen<{ error: string }>('recording-save-failed', (event) => {
      callback(event.payload.error);
    });
  }

  /**
   * Listen for recording-saved event (audio.mp4 finalized on disk).
   * Use this to refresh audio-player UIs that loaded a meeting before its
   * audio file existed. The DB row is already populated with folder_path
   * via the synchronous stopRecording() return value.
   */
  async onRecordingSaved(callback: (payload: RecordingSavedPayload) => void): Promise<UnlistenFn> {
    return listen<RecordingSavedPayload>('recording-saved', (event) => {
      callback(event.payload);
    });
  }

  /**
   * Listen for chunk-drop-warning event (audio buffer overflow)
   * @param callback - Function to call when chunks are dropped
   * @returns Promise that resolves to unlisten function
   */
  async onChunkDropWarning(callback: (warning: string) => void): Promise<UnlistenFn> {
    return listen<string>('chunk-drop-warning', (event) => {
      callback(event.payload);
    });
  }

  /**
   * Listen for speech-detected event (VAD)
   * @param callback - Function to call when speech is detected
   * @returns Promise that resolves to unlisten function
   */
  async onSpeechDetected(callback: () => void): Promise<UnlistenFn> {
    return listen('speech-detected', callback);
  }
}

// Export singleton instance
export const recordingService = new RecordingService();
