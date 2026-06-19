import { useEffect, useRef, useCallback, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { useConfig } from '@/contexts/ConfigContext';
import { useTranscripts } from '@/contexts/TranscriptContext';

// ── Types ──────────────────────────────────────────────────────────────────

interface MeetingDetectedPayload {
  default_title: string;
  candidate_titles: string[];
}

export interface AutoDetectBannerState {
  visible: boolean;
  mode: 'detect-prompt' | 'stop-prompt';
  initialTitle: string;
  candidateTitles: string[];
}

interface UseAutoDetectProps {
  isRecording: boolean;
  handleRecordingStart: (overrideTitle?: string) => Promise<void>;
  handleRecordingStop: (callApi: boolean) => Promise<void>;
  setIsRecording: (value: boolean) => void;
}

const DETECT_TIMEOUT_SECONDS = 10;
const STOP_TIMEOUT_SECONDS = 10;

// ── Pure decision helpers ──────────────────────────────────────────────────
// Extracted so the regression-prone auto-detect guard logic is testable without
// mounting the hook. The hook passes its current ref/state values; these never
// touch refs, effects, or Tauri.

/** D16/D17: a meeting-detected event should auto-start a recording iff the
 *  feature is on and nothing is already recording. */
export function shouldStartOnDetected(opts: {
  autoDetectMeetings: boolean;
  isRecording: boolean;
}): boolean {
  return opts.autoDetectMeetings && !opts.isRecording;
}

/** D17: when a meeting re-engages mid stop-prompt debounce, the stop-prompt is
 *  dismissed before deciding whether to (re)start. */
export function isStopPromptActiveForRedetect(banner: AutoDetectBannerState): boolean {
  return banner.visible && banner.mode === 'stop-prompt';
}

/** Task 7.5: a meeting-ended event should show the stop-prompt only for a
 *  detector-started recording the user hasn't chosen to keep. */
export function shouldShowStopPrompt(opts: {
  autoDetectMeetings: boolean;
  isRecording: boolean;
  isDetectorStarted: boolean;
  isUserManaged: boolean;
}): boolean {
  return (
    opts.autoDetectMeetings &&
    opts.isRecording &&
    opts.isDetectorStarted &&
    !opts.isUserManaged
  );
}

export function detectPromptBanner(payload: MeetingDetectedPayload): AutoDetectBannerState {
  return {
    visible: true,
    mode: 'detect-prompt',
    initialTitle: payload.default_title,
    candidateTitles: payload.candidate_titles,
  };
}

export function stopPromptBanner(): AutoDetectBannerState {
  return { visible: true, mode: 'stop-prompt', initialTitle: '', candidateTitles: [] };
}

/** On detect-prompt confirm, push the edited title to the recording manager
 *  only when it actually changed. */
export function shouldPushTitleUpdate(title: string, currentInitial: string): boolean {
  return !!title && title !== currentInitial;
}

// ── Hook ───────────────────────────────────────────────────────────────────

export function useAutoDetect({
  isRecording,
  handleRecordingStart,
  handleRecordingStop,
  setIsRecording,
}: UseAutoDetectProps) {
  const { autoDetectMeetings } = useConfig();
  const { setMeetingTitle, activeMeetingId } = useTranscripts();

  const [banner, setBanner] = useState<AutoDetectBannerState>({
    visible: false,
    mode: 'detect-prompt',
    initialTitle: '',
    candidateTitles: [],
  });

  // Mutable refs so callbacks always see current values without re-subscribing
  const isRecordingRef = useRef(isRecording);
  const isDetectorStartedRef = useRef(false); // true = this recording was auto-started
  const isUserManagedRef = useRef(false);     // true = user chose "Keep Recording", skip auto-stop
  const bannerRef = useRef(banner);

  // Sync refs at render time, not in an effect. Effects run after the DOM commit;
  // a Tauri event (meeting-ended) can arrive between setState(false) and the effect
  // flush, reading stale truthy refs and wrongly showing the stop-prompt.
  isRecordingRef.current = isRecording;
  if (!isRecording) {
    isDetectorStartedRef.current = false;
    isUserManagedRef.current = false;
  }

  useEffect(() => { bannerRef.current = banner; }, [banner]);

  // Dismiss the banner when recording ends externally. The ref resets above are
  // synchronous; only the visual side-effect (banner dismiss) belongs in an effect.
  useEffect(() => {
    if (!isRecording) {
      setBanner(prev => prev.visible ? { ...prev, visible: false } : prev);
    }
  }, [isRecording]);

  // ── Banner helpers ────────────────────────────────────────────────────────

  const dismissBanner = useCallback(() => {
    setBanner(prev => ({ ...prev, visible: false }));
  }, []);

  // ── Event handlers ────────────────────────────────────────────────────────

  const handleDetected = useCallback(async (payload: MeetingDetectedPayload) => {
    if (!autoDetectMeetings) return;

    // D17 / Task 7.4: dismiss an active stop-prompt — the call re-engaged within the debounce window
    if (isStopPromptActiveForRedetect(bannerRef.current)) {
      setBanner(prev => ({ ...prev, visible: false }));
      isUserManagedRef.current = false;
      // Don't return — fall through: if we're still recording, guard below will catch it
    }

    // D17 / Task 7.5: don't double-start if already recording (manual or detector)
    if (!shouldStartOnDetected({ autoDetectMeetings, isRecording: isRecordingRef.current })) return;

    try {
      await handleRecordingStart(payload.default_title);
      isDetectorStartedRef.current = true;

      // Show the detect-prompt banner so the user can review/edit the title
      setBanner(detectPromptBanner(payload));
    } catch (err) {
      console.error('[useAutoDetect] Failed to auto-start recording:', err);
    }
  }, [autoDetectMeetings, handleRecordingStart]);

  const handleEnded = useCallback(() => {
    if (!shouldShowStopPrompt({
      autoDetectMeetings,
      isRecording: isRecordingRef.current,
      isDetectorStarted: isDetectorStartedRef.current,
      isUserManaged: isUserManagedRef.current,
    })) return;

    setBanner(stopPromptBanner());
  }, [autoDetectMeetings]);

  // ── Banner action handlers ─────────────────────────────────────────────────

  // onConfirm fires from the banner (explicit click or countdown expiry)
  const handleBannerConfirm = useCallback(async (title: string) => {
    const currentMode = bannerRef.current.mode;
    const currentInitial = bannerRef.current.initialTitle;
    dismissBanner();

    if (currentMode === 'detect-prompt') {
      // If user edited the title, push update to the Rust recording manager
      if (shouldPushTitleUpdate(title, currentInitial)) {
        setMeetingTitle(title);
        try {
          await invoke('set_active_meeting_name', { name: title });
        } catch (err) {
          console.warn('[useAutoDetect] set_active_meeting_name failed:', err);
        }
      }
      // Recording continues; nothing else to do here
    } else {
      // stop-prompt confirmed (or timed out): stop and save
      isDetectorStartedRef.current = false;
      try {
        await handleRecordingStop(true);
      } catch (err) {
        console.error('[useAutoDetect] handleRecordingStop failed:', err);
      }
    }
  }, [dismissBanner, setMeetingTitle, handleRecordingStop]);

  // onCancel fires from the banner Dismiss / Keep Recording buttons
  const handleBannerCancel = useCallback(async () => {
    const currentMode = bannerRef.current.mode;
    dismissBanner();

    if (currentMode === 'detect-prompt') {
      // User dismissed the auto-start: cancel the recording and suppress re-detection (D16)
      isDetectorStartedRef.current = false;
      try {
        await invoke('signal_cancel_detection');
      } catch {
        // best effort — even if this fails the recording is cancelled
      }
      try {
        await invoke('cancel_recording', { meeting_id: activeMeetingId || '' });
      } catch (err) {
        console.error('[useAutoDetect] cancel_recording failed:', err);
      }
      // cancel_recording omits recording-stopped — that event triggers the meeting-save flow
      // against a folder that was just deleted.
      setIsRecording(false);
    } else {
      // "Keep Recording": the user wants to continue manually — stop auto-stop prompts
      isUserManagedRef.current = true;
    }
  }, [dismissBanner]);

  // ── Tauri event wiring ─────────────────────────────────────────────────────

  useEffect(() => {
    let unlistenDetected: (() => void) | undefined;
    let unlistenEnded: (() => void) | undefined;

    const setup = async () => {
      unlistenDetected = await listen<MeetingDetectedPayload>('meeting-detected', event => {
        handleDetected(event.payload);
      });
      unlistenEnded = await listen<void>('meeting-ended', () => {
        handleEnded();
      });
    };

    setup().catch(console.error);

    return () => {
      unlistenDetected?.();
      unlistenEnded?.();
    };
  }, [handleDetected, handleEnded]);

  return {
    banner,
    detectTimeoutSeconds: DETECT_TIMEOUT_SECONDS,
    stopTimeoutSeconds: STOP_TIMEOUT_SECONDS,
    handleBannerConfirm,
    handleBannerCancel,
  };
}
