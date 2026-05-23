'use client';

import React, { useState, useEffect, useCallback, useRef } from 'react';
import { toast } from 'sonner';
import {
  SchedulingMode,
  SchedulerSettings,
  defaultSchedulerSettings,
  getSchedulerSettings,
  saveSchedulerSettings,
  validateSchedulerSettings,
} from '@/services/schedulerSettingsService';

const MODES: { value: SchedulingMode; label: string }[] = [
  { value: 'aggressive', label: 'Aggressive' },
  { value: 'polite', label: 'Polite' },
  { value: 'manual', label: 'Manual' },
];

const MODE_HINTS: Record<SchedulingMode, string> = {
  aggressive: 'Runs immediately after recording. CPU & RAM limits ignored.',
  polite: 'Pauses when CPU or RAM exceeds thresholds, resumes automatically.',
  manual: 'Never starts automatically. Use "Run now" per meeting.',
};

function SegmentedControl({
  options,
  value,
  onChange,
}: {
  options: { value: SchedulingMode; label: string }[];
  value: SchedulingMode;
  onChange: (v: SchedulingMode) => void;
}) {
  const activeIndex = options.findIndex((o) => o.value === value);

  const cycle = (dir: 1 | -1) => {
    const next = (activeIndex + dir + options.length) % options.length;
    onChange(options[next].value);
  };

  return (
    <div
      className="inline-flex p-1 bg-gray-100 rounded-xl relative min-w-[320px]"
      role="radiogroup"
      aria-label="Scheduling mode"
    >
      <div
        className="absolute top-1 bottom-1 rounded-lg bg-white shadow-sm transition-[left] duration-200 ease-out motion-reduce:transition-none"
        style={{
          width: `calc(${100 / options.length}% - 4px)`,
          left: `calc(${(100 / options.length) * activeIndex}% + 2px)`,
        }}
      />
      {options.map((opt) => (
        <button
          key={opt.value}
          role="radio"
          aria-checked={value === opt.value}
          onClick={() => onChange(opt.value)}
          onKeyDown={(e) => {
            if (e.key === 'ArrowRight' || e.key === 'ArrowDown') { e.preventDefault(); cycle(1); }
            if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') { e.preventDefault(); cycle(-1); }
          }}
          className={`relative z-10 flex-1 px-5 py-2 text-sm font-medium rounded-lg cursor-pointer text-center transition-colors duration-100 outline-none focus-visible:ring-2 focus-visible:ring-blue-500 focus-visible:ring-offset-1 motion-reduce:transition-none ${
            value === opt.value
              ? 'text-gray-900'
              : 'text-gray-500 hover:text-gray-700'
          }`}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}

let numericInputIdCounter = 0;

function NumericInput({
  label,
  value,
  onChange,
  onCommit,
  error,
  min,
  max,
  unit,
}: {
  label: string;
  value: number;
  onChange: (v: number) => void;
  onCommit: () => void;
  error?: string;
  min: number;
  max: number;
  unit?: string;
}) {
  const [inputId] = useState(() => `numeric-input-${numericInputIdCounter++}`);

  return (
    <div className="space-y-1.5">
      <label htmlFor={inputId} className="text-sm font-medium text-gray-700">
        {label}
      </label>
      <div className="flex items-center gap-2">
        <input
          id={inputId}
          type="number"
          min={min}
          max={max}
          value={value}
          onChange={(e) => {
            const v = parseInt(e.target.value, 10);
            if (!isNaN(v)) onChange(v);
          }}
          onBlur={onCommit}
          className={`w-20 px-3 py-1.5 border rounded-lg text-sm text-center tabular-nums focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent ${
            error ? 'border-red-400' : 'border-gray-300'
          }`}
        />
        {unit && <span className="text-sm text-gray-500">{unit}</span>}
      </div>
      {error && <p className="text-xs text-red-600" role="alert">{error}</p>}
    </div>
  );
}

export function BackgroundProcessingSettings() {
  const [settings, setSettings] = useState<SchedulerSettings>(defaultSchedulerSettings());
  const [loading, setLoading] = useState(true);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const modeSwitching = useRef(false);

  useEffect(() => {
    getSchedulerSettings()
      .then((s) => setSettings(s))
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const persist = useCallback(
    async (updated: SchedulerSettings, silent = false) => {
      const validationErrors = validateSchedulerSettings(updated);
      setErrors(validationErrors);
      if (Object.keys(validationErrors).length > 0) return;
      try {
        await saveSchedulerSettings(updated);
        setSettings(updated);
        if (!silent) {
          toast.success('Background processing settings saved');
        }
      } catch (err) {
        toast.error('Failed to save settings', {
          description: err instanceof Error ? err.message : String(err),
        });
      } finally {
        modeSwitching.current = false;
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

  const mode = settings.scheduling_mode;

  return (
    <div className="space-y-5">
      <div>
        <h3 className="text-lg font-semibold mb-1">Background Processing</h3>
        <p className="text-sm text-gray-600">
          Control when transcription and summarisation run after a meeting ends.
        </p>
      </div>

      <SegmentedControl
        options={MODES}
        value={mode}
        onChange={(m) => {
          modeSwitching.current = true;
          persist({ ...settings, scheduling_mode: m }, true);
        }}
      />

      <p className="text-sm text-gray-500 leading-relaxed" aria-live="polite">
        {MODE_HINTS[mode]}
      </p>

      {mode === 'polite' && (
        <div className="grid grid-cols-2 gap-x-6 gap-y-4 p-4 border border-gray-200 rounded-xl bg-gray-50/50">
          <NumericInput
            label="CPU threshold"
            value={settings.cpu_pause_threshold_pct}
            onChange={(v) =>
              setSettings({ ...settings, cpu_pause_threshold_pct: v })
            }
            onCommit={() => {
              if (modeSwitching.current) return;
              persist({ ...settings, cpu_pause_threshold_pct: settings.cpu_pause_threshold_pct });
            }}
            error={errors.cpu_pause_threshold_pct}
            min={1}
            max={100}
            unit="%"
          />
          <NumericInput
            label="CPU sustained"
            value={settings.cpu_pause_duration_secs}
            onChange={(v) =>
              setSettings({ ...settings, cpu_pause_duration_secs: v })
            }
            onCommit={() => {
              if (modeSwitching.current) return;
              persist({ ...settings, cpu_pause_duration_secs: settings.cpu_pause_duration_secs });
            }}
            error={errors.cpu_pause_duration_secs}
            min={5}
            max={600}
            unit="seconds"
          />
          <NumericInput
            label="RAM threshold"
            value={settings.ram_pause_threshold_pct}
            onChange={(v) =>
              setSettings({ ...settings, ram_pause_threshold_pct: v })
            }
            onCommit={() => {
              if (modeSwitching.current) return;
              persist({ ...settings, ram_pause_threshold_pct: settings.ram_pause_threshold_pct });
            }}
            error={errors.ram_pause_threshold_pct}
            min={1}
            max={100}
            unit="%"
          />
          <NumericInput
            label="RAM sustained"
            value={settings.ram_pause_duration_secs}
            onChange={(v) =>
              setSettings({ ...settings, ram_pause_duration_secs: v })
            }
            onCommit={() => {
              if (modeSwitching.current) return;
              persist({ ...settings, ram_pause_duration_secs: settings.ram_pause_duration_secs });
            }}
            error={errors.ram_pause_duration_secs}
            min={5}
            max={600}
            unit="seconds"
          />
        </div>
      )}
    </div>
  );
}
