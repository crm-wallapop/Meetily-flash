'use client';

import { useState, useEffect, useCallback } from 'react';
import { toast } from 'sonner';
import {
  getMeetingMaxSpeakers,
  setMeetingMaxSpeakers,
  type MeetingMaxSpeakers,
} from '@/services/speakerService';

export const MEETING_CAP_MIN = 2;
export const MEETING_CAP_MAX = 20;

export function isValidMeetingCap(n: number): boolean {
  return Number.isInteger(n) && n >= MEETING_CAP_MIN && n <= MEETING_CAP_MAX;
}

export function MeetingMaxSpeakersControl({ meetingId }: { meetingId: string }) {
  const [state, setState] = useState<MeetingMaxSpeakers | null>(null);
  const [draft, setDraft] = useState<string>('');
  const [auto, setAuto] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const s = await getMeetingMaxSpeakers(meetingId);
    setState(s);
    setAuto(s.override === null);
    setDraft(s.override !== null ? String(s.override) : String(s.effective));
    return s;
  }, [meetingId]);

  useEffect(() => {
    refresh().catch(() => {});
  }, [refresh]);

  const commit = useCallback(
    async (cap: number | null) => {
      setError(null);
      try {
        await setMeetingMaxSpeakers(meetingId, cap);
        const s = await refresh();
        toast.success(
          cap === null
            ? `Using default max speakers (${s.global_default})`
            : `Max speakers set to ${cap} (applies on next Re-diarize)`,
        );
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [meetingId, refresh],
  );

  const onToggleAuto = useCallback(() => {
    if (auto) {
      setAuto(false);
      return;
    }
    setAuto(true);
    commit(null);
  }, [auto, commit]);

  const onCommitDraft = useCallback(() => {
    const v = parseInt(draft, 10);
    if (!isValidMeetingCap(v)) {
      setError(`Max speakers must be between ${MEETING_CAP_MIN} and ${MEETING_CAP_MAX}`);
      return;
    }
    commit(v);
  }, [draft, commit]);

  if (!state) {
    return (
      <div className="animate-pulse">
        <div className="h-4 bg-gray-200 rounded w-24" />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-1" data-testid="meeting-max-speakers-control">
      <div className="flex items-center gap-2">
        <label className="text-xs font-medium text-gray-600 whitespace-nowrap">
          Max speakers
        </label>
        <input
          type="number"
          min={MEETING_CAP_MIN}
          max={MEETING_CAP_MAX}
          value={draft}
          disabled={auto}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={onCommitDraft}
          onKeyDown={(e) => {
            if (e.key === 'Enter') onCommitDraft();
          }}
          className="w-16 px-2 py-1 border border-gray-200 rounded-md text-xs text-center focus:outline-none focus:ring-1 focus:ring-blue-500 disabled:bg-gray-100 disabled:text-gray-400"
        />
        <button
          type="button"
          onClick={onToggleAuto}
          className={`text-xs px-2 py-1 rounded-md border whitespace-nowrap ${
            auto
              ? 'bg-blue-50 border-blue-200 text-blue-700'
              : 'bg-white border-gray-200 text-gray-500 hover:bg-gray-50'
          }`}
          title={
            auto
              ? `Using global default (${state.global_default})`
              : 'Using a per-meeting override'
          }
        >
          {auto ? `Auto (${state.global_default})` : 'Override'}
        </button>
      </div>
      {error && <p className="text-xs text-red-600">{error}</p>}
    </div>
  );
}
