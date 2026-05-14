'use client';

import { motion } from 'framer-motion';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';

interface RecordingStatusBarProps {
  isPaused?: boolean;
}

export const RecordingStatusBar: React.FC<RecordingStatusBarProps> = ({ isPaused = false }) => {
  const { activeDuration, isRecording, isSaving } = useRecordingState();

  // Display state synced from backend
  const [displaySeconds, setDisplaySeconds] = useState(0);
  const [gainDb, setGainDb] = useState<number | null>(null);

  // Sync with backend duration when it changes (handles refresh/navigation)
  useEffect(() => {
    if (activeDuration !== null) {
      // Round to nearest second to avoid decimal issues
      setDisplaySeconds(Math.floor(activeDuration));
    }
  }, [activeDuration]);

  // Listen for normalizer gain events while recording; clear gain when recording stops.
  // Cancellation flag guards against the race where cleanup runs before listen() resolves.
  useEffect(() => {
    if (!isRecording) {
      setGainDb(null);
      return;
    }
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen<{ gain_db: number }>('audio-normalizer-gain', (event) => {
      setGainDb(event.payload.gain_db);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [isRecording]);

  const formatDuration = (seconds: number): string => {
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  };

  if (isSaving) {
    return (
      <motion.div
        initial={{ opacity: 0, y: -10 }}
        animate={{ opacity: 1, y: 0 }}
        exit={{ opacity: 0, y: -10 }}
        transition={{ duration: 0.2 }}
        className="flex items-center gap-2 px-3 py-2 bg-gray-50 rounded-lg mb-2"
        data-testid="saving-status-bar"
      >
        {/* Gray spinner — distinct from the red recording dot */}
        <svg
          className="w-3 h-3 animate-spin text-gray-400"
          xmlns="http://www.w3.org/2000/svg"
          fill="none"
          viewBox="0 0 24 24"
          aria-hidden="true"
          data-testid="saving-spinner"
        >
          <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
          <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z" />
        </svg>
        <span className="text-sm text-gray-500" data-testid="saving-label">Saving…</span>
      </motion.div>
    );
  }

  return (
    <motion.div
      initial={{ opacity: 0, y: -10 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -10 }}
      transition={{ duration: 0.2 }}
      className="flex items-center gap-2 px-3 py-2 bg-gray-50 rounded-lg mb-2"
    >
      <div className={`w-2 h-2 rounded-full ${isPaused ? 'bg-orange-500' : 'bg-red-500 animate-pulse'}`} />
      <span className={`text-sm ${isPaused ? 'text-orange-700' : 'text-gray-700'}`}>
        {isPaused ? 'Paused' : 'Recording'} • {formatDuration(displaySeconds)}
      </span>
      <span className="text-xs text-gray-400 ml-1">
        Gain: {gainDb !== null ? `${gainDb >= 0 ? '+' : ''}${gainDb.toFixed(1)} dB` : '—'}
      </span>
    </motion.div>
  );
};
