'use client';

import { useState, useEffect, useCallback } from 'react';
import { toast } from 'sonner';
import {
  getSpeakerMergeThreshold,
  setSpeakerMergeThreshold,
  getMaxSpeakers,
  setMaxSpeakers,
  getDiarizationEnabled,
  setDiarizationEnabled,
} from '@/services/speakerService';

const MERGE_MIN = 0.35;
const MERGE_MAX = 0.70;
const MERGE_STEP = 0.05;
const CAP_MIN = 2;
const CAP_MAX = 20;

const THRESHOLD_HINTS: Record<string, string> = {
  low: 'More speakers detected. Useful when speakers have very different voices.',
  default: 'Balanced. Works well for most meetings.',
  high: 'Fewer speakers. Useful when the same person sounds different across segments.',
};

function hintFor(value: number) {
  if (value < 0.45) return THRESHOLD_HINTS.low;
  if (value > 0.55) return THRESHOLD_HINTS.high;
  return THRESHOLD_HINTS.default;
}

export function SpeakerMergeThresholdSlider() {
  const [threshold, setThreshold] = useState(0.40);
  const [saved, setSaved] = useState(0.40);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    getSpeakerMergeThreshold()
      .then((t) => {
        setThreshold(t);
        setSaved(t);
      })
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const commit = useCallback(
    async (value: number) => {
      try {
        await setSpeakerMergeThreshold(value);
        setSaved(value);
        toast.success('Speaker merge threshold saved');
      } catch (err) {
        toast.error('Failed to save threshold', {
          description: err instanceof Error ? err.message : String(err),
        });
      }
    },
    [],
  );

  if (loading) {
    return (
      <div className="animate-pulse space-y-3 pt-6">
        <div className="h-4 bg-gray-200 rounded w-1/3" />
        <div className="h-8 bg-gray-200 rounded-xl w-80" />
      </div>
    );
  }

  return (
    <div className="pt-6 space-y-3">
      <div className="flex items-center justify-between">
        <label className="text-sm font-medium text-gray-700">
          Speaker merge similarity
        </label>
        <span className="text-sm tabular-nums font-mono text-gray-900">
          {threshold.toFixed(2)}
        </span>
      </div>

      <input
        type="range"
        min={MERGE_MIN}
        max={MERGE_MAX}
        step={MERGE_STEP}
        value={threshold}
        onChange={(e) => setThreshold(parseFloat(e.target.value))}
        onMouseUp={() => commit(threshold)}
        onTouchEnd={() => commit(threshold)}
        className="w-full h-2 bg-gray-200 rounded-lg appearance-none cursor-pointer accent-blue-600"
      />

      <div className="flex justify-between text-xs text-gray-400 tabular-nums">
        <span>{MERGE_MIN.toFixed(2)} (more speakers)</span>
        <span>{MERGE_MAX.toFixed(2)} (fewer speakers)</span>
      </div>

      <p className="text-sm text-gray-500" aria-live="polite">
        {hintFor(threshold)}
      </p>
    </div>
  );
}

function MaxSpeakersInput() {
  const [cap, setCap] = useState(10);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getMaxSpeakers()
      .then((v) => {
        setCap(v);
      })
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const commit = useCallback(async (value: number) => {
    if (!Number.isInteger(value) || value < CAP_MIN || value > CAP_MAX) {
      setError(`Max speakers must be between ${CAP_MIN} and ${CAP_MAX}`);
      return;
    }
    setError(null);
    try {
      await setMaxSpeakers(value);
      toast.success('Max speakers saved');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  if (loading) {
    return (
      <div className="animate-pulse space-y-3">
        <div className="h-4 bg-gray-200 rounded w-1/4" />
        <div className="h-9 bg-gray-200 rounded-lg w-24" />
      </div>
    );
  }

  return (
    <div className="space-y-2">
      <label className="text-sm font-medium text-gray-700">
        Max speakers
      </label>
      <div className="flex items-center gap-3">
        <input
          type="number"
          min={CAP_MIN}
          max={CAP_MAX}
          value={cap}
          onChange={(e) => {
            const v = parseInt(e.target.value, 10);
            if (!isNaN(v)) setCap(v);
          }}
          onBlur={() => commit(cap)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') commit(cap);
          }}
          className="w-20 px-3 py-2 border border-gray-200 rounded-lg text-sm text-center focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500"
        />
        <span className="text-xs text-gray-400">
          ({CAP_MIN}–{CAP_MAX})
        </span>
      </div>
      {error && (
        <p className="text-xs text-red-600">{error}</p>
      )}
    </div>
  );
}

export function SpeakerSettings() {
  const [enabled, setEnabled] = useState(true);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    getDiarizationEnabled()
      .then(setEnabled)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const toggleEnabled = useCallback(async (value: boolean) => {
    try {
      await setDiarizationEnabled(value);
      setEnabled(value);
      toast.success(value ? 'Speaker detection enabled' : 'Speaker detection disabled');
    } catch (err) {
      toast.error('Failed to update setting', {
        description: err instanceof Error ? err.message : String(err),
      });
    }
  }, []);

  return (
    <div className="space-y-5">
      <div>
        <h3 className="text-lg font-semibold mb-1">Speaker Detection</h3>
        <p className="text-sm text-gray-600">
          Identify and label different speakers in your meetings.
        </p>
      </div>
      {!loading && (
        <div className="p-4 border border-gray-200 rounded-xl bg-gray-50/50 space-y-4">
          <div className="flex items-center justify-between">
            <div>
              <p className="text-sm font-medium text-gray-700">Enable speaker detection</p>
              <p className="text-xs text-gray-500">
                Run speaker identification after each recording, import, or retranscription.
              </p>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={enabled}
              onClick={() => toggleEnabled(!enabled)}
              className={`relative inline-flex h-6 w-11 shrink-0 cursor-pointer rounded-full border-2 border-transparent transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 ${
                enabled ? 'bg-blue-600' : 'bg-gray-200'
              }`}
            >
              <span
                className={`pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0 transition duration-200 ease-in-out ${
                  enabled ? 'translate-x-5' : 'translate-x-0'
                }`}
              />
            </button>
          </div>

          {enabled && (
            <MaxSpeakersInput />
          )}
        </div>
      )}
      {enabled && (
        <div className="border-t border-gray-200 pt-5">
          <h4 className="text-sm font-semibold text-gray-800 mb-1">Merge Threshold</h4>
          <p className="text-xs text-gray-500 mb-3">
            Controls how similar voice segments must be to merge into one speaker.
          </p>
          <SpeakerMergeThresholdSlider />
        </div>
      )}
    </div>
  );
}
