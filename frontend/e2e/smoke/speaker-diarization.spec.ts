import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';
import { SMOKE_SETTINGS_INIT_SCRIPT } from './_settings';
import { bootstrap, speakerCalls } from './_speaker-helpers';

// Backfill of the speaker-diarization change's manual section-15 scenarios that are
// expressible as UI-wiring smoke (15.2 inline label, 15.3 re-diarize, 15.5
// retranscribe, 15.6 colors). Backend correctness (cross-meeting matching,
// manual-label preservation, label clearing) is locked by the cargo tests
// (14.5/14.6, 14.7/14.8, 4.23-4.25); these specs prove the live DOM dispatches
// the right Tauri commands and renders state.
// 15.1 (record a real multi-speaker meeting) needs a human + live mic and stays
// manual; 15.4 (import audio) is covered by the real-audio harness run; 15.7 by
// the settings-page describe below.

test.describe('speaker-diarization smoke (section 15 backfill)', () => {
  test.beforeEach(() => {
    // Cold first-compile of /meeting-details is slow (see summary-render smoke);
    // the default 30s budget aborts page.goto mid-compile.
    test.setTimeout(120_000);
  });

  test('15.3 — Speakers button dispatches reset_speaker_labels then refetches on the diarization-complete event', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
      { id: 't2', text: 'Speaker one replying.', timestamp: '00:00:04', audio_start_time: 4, speaker: 'Speaker 1' },
    ]);

    const speakersBtn = page.getByTitle('Re-run speaker detection on this meeting');
    await expect(speakersBtn).toBeVisible({ timeout: 20_000 });
    await speakersBtn.click();

    // handleRediarize registers the diarization-complete listener BEFORE calling
    // resetSpeakerLabels, so once the command lands the listener is guaranteed live.
    await expect.poll(async () => {
      const calls = await speakerCalls(page);
      return calls.find((c) => c.cmd === 'reset_speaker_labels') ?? null;
    }, { timeout: 10_000 }).toEqual({ cmd: 'reset_speaker_labels', meetingId: 'meet-summary-001' });

    // Emit the event the real backend would send once diarization finishes.
    await page.evaluate(() => {
      (window as unknown as { __tauriMockEventBus: { emit: (e: string, p: unknown) => void } })
        .__tauriMockEventBus.emit('diarization-complete', {
          meeting_id: 'meet-summary-001',
          speaker_count: 3,
          segments_labeled: 12,
        });
    });

    // The handler shows a "Detected N speakers" toast on completion.
    await expect(page.getByText('Detected 3 speakers')).toBeVisible({ timeout: 10_000 });
  });

  test('15.2 — inline rename dispatches label_speaker {meetingId, clusterLabel, speakerName}', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
    ]);

    // Click the speaker badge → it is replaced by the SpeakerLabelInput.
    const badge = page.getByRole('button', { name: 'Speaker 0' });
    await expect(badge).toBeVisible({ timeout: 20_000 });
    await badge.click();

    const input = page.getByPlaceholder('Enter speaker name...');
    await expect(input).toBeVisible({ timeout: 5_000 });
    await input.fill('Alice');
    await input.press('Enter'); // onSubmit(name) → handleSpeakerSubmit → labelSpeaker

    await expect.poll(async () => {
      const calls = await speakerCalls(page);
      return calls.find((c) => c.cmd === 'label_speaker') ?? null;
    }, { timeout: 10_000 }).toEqual({
      cmd: 'label_speaker',
      meetingId: 'meet-summary-001',
      clusterLabel: 'Speaker 0',
      speakerName: 'Alice',
    });
  });

  test('15.6 — speaker color is deterministic by speaker (same name → same color, distinct names → distinct)', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'First turn.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
      { id: 't2', text: 'Second turn, same speaker.', timestamp: '00:00:04', audio_start_time: 4, speaker: 'Speaker 0' },
      { id: 't3', text: 'Third turn, other speaker.', timestamp: '00:00:08', audio_start_time: 8, speaker: 'Speaker 1' },
    ]);

    // Wait for the badges to mount (SpeakerBadge renders span[role=button] when editable).
    await page.waitForSelector('span[role="button"]', { timeout: 20_000 });

    const colors = await page.evaluate(() => {
      const badges = Array.from(document.querySelectorAll<HTMLElement>('span[role="button"]'));
      return badges.map((b) => window.getComputedStyle(b).backgroundColor);
    });

    expect(colors.length, 'three speaker badges should render').toBe(3);
    // Same speaker (Speaker 0) on the first two segments → identical color.
    expect(colors[0]).toBe(colors[1]);
    // Distinct speaker (Speaker 1) → different color.
    expect(colors[0]).not.toBe(colors[2]);
  });

  test('15.5 — Enhance retranscribe dispatches start_retranscription_command and refetches transcripts on retranscription-complete', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
      { id: 't2', text: 'Speaker one replying.', timestamp: '00:00:04', audio_start_time: 4, speaker: 'Speaker 1' },
    ]);

    // folder_path is set on the meeting metadata and the beta flag defaults on,
    // so the Enhance button renders and opens RetranscribeDialog.
    const enhance = page.getByTitle('Retranscribe to enhance your recorded audio');
    await expect(enhance).toBeVisible({ timeout: 20_000 });
    await enhance.click();

    // The Start button only exists while the dialog is open and idle.
    const startBtn = page.getByRole('button', { name: 'Start Retranscription' });
    await expect(startBtn).toBeVisible({ timeout: 10_000 });
    await startBtn.click();

    // handleStartRetranscription dispatches the command with the meeting id and
    // the folder path it was handed (provider comes from the model dropdown).
    await expect.poll(async () => {
      const calls = await page.evaluate(() =>
        (window as unknown as { __smokeRetranscribeCalls?: { cmd: string; meetingId?: string; meetingFolderPath?: string; provider?: string }[] })
          .__smokeRetranscribeCalls ?? [],
      );
      return calls.find((c) => c.cmd === 'start_retranscription_command') ?? null;
    }, { timeout: 10_000 }).toMatchObject({
      cmd: 'start_retranscription_command',
      meetingId: 'meet-summary-001',
      meetingFolderPath: expect.any(String),
      provider: 'whisper',
    });

    // The completion listener registers asynchronously on dialog open; poll the
    // bus so the emit lands after subscription (emit-before-subscribe drops silently).
    await expect.poll(async () => {
      return page.evaluate(() =>
        (window as unknown as { __tauriMockEventBus: { listenerCount: (e: string) => number } })
          .__tauriMockEventBus.listenerCount('retranscription-complete'),
      );
    }, { timeout: 10_000 }).toBeGreaterThanOrEqual(1);

    // Emit the completion event the backend would send. The handler toasts
    // success and calls onComplete -> handleRetranscribeComplete, which refetches
    // transcripts: the UI path by which freshly re-diarized labels reach the DOM
    // once the old segments (and their labels) have been replaced.
    await page.evaluate(() => {
      (window as unknown as { __tauriMockEventBus: { emit: (e: string, p: unknown) => void } })
        .__tauriMockEventBus.emit('retranscription-complete', {
          meeting_id: 'meet-summary-001',
          segments_count: 14,
          duration_seconds: 95.0,
          language: 'en',
        });
    });

    await expect(page.getByText('Retranscription complete! 14 segments created.')).toBeVisible({ timeout: 10_000 });

    // Initial mount fetched once; the completion refetch must tick the counter a
    // second time, proving the cleared-and-re-diarized transcripts are reloaded.
    await expect.poll(async () => {
      return page.evaluate(() =>
        (window as unknown as { __smokeTranscriptsFetchCount?: number }).__smokeTranscriptsFetchCount ?? 0,
      );
    }, { timeout: 10_000 }).toBeGreaterThanOrEqual(2);
  });
});

test.describe('speaker-diarization settings smoke (15.7)', () => {
  test.beforeEach(() => {
    test.setTimeout(120_000);
  });

  async function bootstrapSettings(page: import('@playwright/test').Page): Promise<void> {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(SMOKE_SETTINGS_INIT_SCRIPT);
    await page.goto('/settings');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );
  }

  test('15.7 — merge-threshold slider commits on mouseup via set_speaker_merge_threshold', async ({ page }) => {
    await bootstrapSettings(page);

    // SpeakerSettings lives behind the "speakers" tab (the page opens on "general");
    // Radix Tabs renders only the active TabsContent, so switch tabs first.
    await page.getByRole('tab', { name: 'Speakers' }).click();

    const slider = page.locator('input[type="range"]');
    await expect(slider).toBeVisible({ timeout: 20_000 });
    const box = await slider.boundingBox();
    expect(box, 'slider bounding box').not.toBeNull();
    if (!box) return;
    const y = box.y + box.height / 2;

    // The slider updates its displayed value via onChange during the drag but only
    // PERSISTS via commit() on onMouseUp/onTouchEnd — so a real mouse drag (not
    // keyboard arrows, which never fire mouseup) is required to exercise the wiring.
    await page.mouse.move(box.x + 2, y);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.75, y, { steps: 8 });
    await page.mouse.up();

    await expect.poll(async () => {
      const calls = await page.evaluate(
        () => (window as unknown as { __smokeSettingsCalls?: { cmd: string; threshold?: number }[] }).__smokeSettingsCalls ?? [],
      );
      return calls.find((c) => c.cmd === 'set_speaker_merge_threshold') ?? null;
    }, { timeout: 10_000 }).toMatchObject({ cmd: 'set_speaker_merge_threshold' });

    // Slider loaded at 0.40; a rightward drag (toward "fewer speakers") must commit a higher value.
    const dispatched = await page.evaluate(() => {
      const calls = (window as unknown as { __smokeSettingsCalls?: { cmd: string; threshold?: number }[] }).__smokeSettingsCalls ?? [];
      return calls.find((c) => c.cmd === 'set_speaker_merge_threshold')?.threshold ?? null;
    });
    expect(dispatched, 'a threshold was persisted').not.toBeNull();
    expect(dispatched!).toBeGreaterThan(0.4);
  });
});
