'use client';

import { useState, useEffect, useCallback } from 'react';
import { toast } from 'sonner';
import {
  getSpeakerMergeThreshold,
  setSpeakerMergeThreshold,
} from '@/services/speakerService';

const MIN = 0.30;
const MAX = 0.70;
const STEP = 0.05;

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

export function SpeakerSettings() {
  const [threshold, setThreshold] = useState(0.50);
  const [saved, setSaved] = useState<number | null>(null);
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
      <div className="animate-pulse space-y-4">
        <div className="h-4 bg-gray-200 rounded w-1/3" />
        <div className="h-10 bg-gray-200 rounded-xl w-80" />
      </div>
    );
  }

  const dirty = saved === null || Math.abs(threshold - saved) > 0.001;

  return (
    <div className="space-y-5">
      <div>
        <h3 className="text-lg font-semibold mb-1">Speaker Detection</h3>
        <p className="text-sm text-gray-600">
          Adjust how aggressively similar voice segments are merged into the same speaker.
        </p>
      </div>

      <div className="p-4 border border-gray-200 rounded-xl bg-gray-50/50 space-y-4">
        <div className="flex items-center justify-between">
          <label className="text-sm font-medium text-gray-700">
            Merge similarity threshold
          </label>
          <span className="text-sm tabular-nums font-mono text-gray-900">
            {threshold.toFixed(2)}
          </span>
        </div>

        <input
          type="range"
          min={MIN}
          max={MAX}
          step={STEP}
          value={threshold}
          onChange={(e) => setThreshold(parseFloat(e.target.value))}
          onMouseUp={() => commit(threshold)}
          onTouchEnd={() => commit(threshold)}
          className="w-full h-2 bg-gray-200 rounded-lg appearance-none cursor-pointer accent-blue-600"
        />

        <div className="flex justify-between text-xs text-gray-400 tabular-nums">
          <span>{MIN.toFixed(2)} (more speakers)</span>
          <span>{MAX.toFixed(2)} (fewer speakers)</span>
        </div>

        <p className="text-sm text-gray-500" aria-live="polite">
          {hintFor(threshold)}
        </p>

        {dirty && (
          <p className="text-xs text-amber-600">
            Unsaved changes — release the slider to save.
          </p>
        )}
      </div>
    </div>
  );
}
