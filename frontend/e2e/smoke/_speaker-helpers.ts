// Shared smoke helpers for speaker-related meeting-details specs. Extracted from
// speaker-diarization.spec.ts so speaker-rename-cancel.spec.ts (and future per-turn
// override specs) reuse the same bootstrap + call-capture wiring.
import type { Page } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';
import { SMOKE_MEETING_DETAILS_INIT_SCRIPT } from './_meeting-details';

export const MEETING_URL = '/meeting-details?id=meet-summary-001';

export interface SmokeTranscript {
  id: string;
  text: string;
  timestamp: string;
  audio_start_time: number;
  speaker?: string;
}

export interface SpeakerCall {
  cmd: string;
  meetingId?: string;
  clusterLabel?: string;
  speakerName?: string;
  speakerLabel?: string;
}

export interface SmokeSpeaker {
  id: string;
  name: string;
  color: string;
}

export async function setTranscripts(
  page: Page,
  transcripts: SmokeTranscript[],
): Promise<void> {
  await page.addInitScript((t) => {
    (window as unknown as { __smokeTranscripts?: SmokeTranscript[] }).__smokeTranscripts = t;
  }, transcripts);
}

export async function setSpeakers(
  page: Page,
  speakers: SmokeSpeaker[],
): Promise<void> {
  await page.addInitScript((s) => {
    (window as unknown as { __smokeSpeakers?: SmokeSpeaker[] }).__smokeSpeakers = s;
  }, speakers);
}

export async function bootstrap(
  page: Page,
  transcripts: SmokeTranscript[],
  speakers?: SmokeSpeaker[],
): Promise<void> {
  page.on('dialog', (d) => d.dismiss());
  await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
  await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
  await page.addInitScript(SMOKE_MEETING_DETAILS_INIT_SCRIPT);
  await setTranscripts(page, transcripts);
  if (speakers) await setSpeakers(page, speakers);
  await page.goto(MEETING_URL);
  await page.waitForFunction(
    () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
    { timeout: 15_000 },
  );
}

export async function speakerCalls(page: Page): Promise<SpeakerCall[]> {
  return page.evaluate(() =>
    (window as unknown as { __smokeSpeakerCalls?: SpeakerCall[] }).__smokeSpeakerCalls ?? [],
  );
}
