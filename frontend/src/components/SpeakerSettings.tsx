'use client';

import { useState, useEffect, useCallback } from 'react';
import { toast } from 'sonner';
import {
  getSpeakerMergeThreshold,
  setSpeakerMergeThreshold,
  getSpeakerEmbeddingModel,
  setSpeakerEmbeddingModel,
  getMaxSpeakers,
  setMaxSpeakers,
  SPEAKER_EMBEDDING_MODELS,
  type SpeakerEmbeddingModel,
} from '@/services/speakerService';

const MERGE_MIN = 0.30;
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
      <div className="animate-pulse space-y-3 pt-6">
        <div className="h-4 bg-gray-200 rounded w-1/3" />
        <div className="h-8 bg-gray-200 rounded-xl w-80" />
      </div>
    );
  }

  const dirty = saved === null || Math.abs(threshold - saved) > 0.001;

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

      {dirty && (
        <p className="text-xs text-amber-600">
          Unsaved changes — release the slider to save.
        </p>
      )}
    </div>
  );
}

function SpeakerModelSelect() {
  const [model, setModel] = useState<SpeakerEmbeddingModel>('3dspeaker');
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    getSpeakerEmbeddingModel()
      .then(setModel)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const handleChange = useCallback(async (value: string) => {
    const m = value as SpeakerEmbeddingModel;
    try {
      await setSpeakerEmbeddingModel(m);
      setModel(m);
      toast.success('Speaker model updated');
    } catch (err) {
      toast.error('Failed to save speaker model', {
        description: err instanceof Error ? err.message : String(err),
      });
    }
  }, []);

  if (loading) {
    return (
      <div className="animate-pulse space-y-3">
        <div className="h-4 bg-gray-200 rounded w-1/4" />
        <div className="h-9 bg-gray-200 rounded-lg w-80" />
      </div>
    );
  }

  return (
    <div className="space-y-2">
      <label className="text-sm font-medium text-gray-700">
        Embedding model
      </label>
      <select
        value={model}
        onChange={(e) => handleChange(e.target.value)}
        className="w-full max-w-xs px-3 py-2 border border-gray-200 rounded-lg text-sm bg-white focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500"
      >
        {SPEAKER_EMBEDDING_MODELS.map((m) => (
          <option key={m.value} value={m.value}>
            {m.label}
          </option>
        ))}
      </select>
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
  return (
    <div className="space-y-5">
      <div>
        <h3 className="text-lg font-semibold mb-1">Speaker Detection</h3>
        <p className="text-sm text-gray-600">
          Adjust how aggressively similar voice segments are merged into the same speaker.
        </p>
      </div>
      <div className="p-4 border border-gray-200 rounded-xl bg-gray-50/50 space-y-4">
        <SpeakerModelSelect />
        <MaxSpeakersInput />
        <SpeakerMergeThresholdSlider />
      </div>
    </div>
  );
}
