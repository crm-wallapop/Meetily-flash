import { listen, UnlistenFn } from '@tauri-apps/api/event';

export interface DiarizationCompletePayload {
  meeting_id: string;
  speaker_count: number;
  segments_labeled: number;
}

export function validateDiarizationPayload(
  raw: unknown,
): DiarizationCompletePayload | null {
  if (raw === null || raw === undefined || typeof raw !== 'object') {
    return null;
  }

  const obj = raw as Record<string, unknown>;

  if (typeof obj.meeting_id !== 'string' || obj.meeting_id === '') {
    return null;
  }

  if (typeof obj.speaker_count !== 'number' || obj.speaker_count < 0) {
    return null;
  }

  if (typeof obj.segments_labeled !== 'number' || obj.segments_labeled < 0) {
    return null;
  }

  return {
    meeting_id: obj.meeting_id,
    speaker_count: obj.speaker_count,
    segments_labeled: obj.segments_labeled,
  };
}

export function onDiarizationComplete(
  callback: (payload: DiarizationCompletePayload) => void,
): Promise<UnlistenFn> {
  return listen<unknown>('diarization-complete', (event) => {
    const validated = validateDiarizationPayload(event.payload);
    if (validated) {
      callback(validated);
    }
  });
}
