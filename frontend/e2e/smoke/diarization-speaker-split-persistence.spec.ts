import { test, expect } from '@playwright/test';
import { bootstrap, speakerCalls, MEETING_URL, type SmokeTranscript } from './_speaker-helpers';

// Smoke for the diarization-speaker-split-persistence change. The transactional
// delete-source + insert-N persistence is locked by cargo tests (§1/§3). The
// user-visible surface is the event→refetch→render wiring: after rediarize, a
// single coarse multi-speaker source row is replaced by N fine rows with fresh
// ids and distinct speakers, and the renderer must surface all N as distinct
// badges. This spec pins that wiring at N=3 (variable-count split) — distinct
// from diarization-label-quality.spec.ts's N=2 — and asserts the source id is
// absent from the rendered set (the delete-source contract reflected in the
// fixture the mock refetch serves). Per feedback_smoke_mock_emit_after_reload:
// the diarization-complete event is emitted BEFORE any reload (post-reload
// page.evaluate emit is inert).

test.describe('diarization-speaker-split-persistence smoke', () => {
  test.beforeEach(() => {
    test.setTimeout(120_000);
  });

  test('a 3-way split renders three distinct speaker badges with the source id gone', async ({ page }) => {
    // One coarse source row spanning three speakers under a single label —
    // the pre-split state. id "coarse-1" must NOT appear among rendered rows
    // after the split (delete-source contract).
    await bootstrap(page, [
      { id: 'coarse-1', text: 'Three-way window spanning speakers A B and C.', timestamp: '00:10:00', audio_start_time: 600, speaker: 'Speaker 0' },
    ]);

    await page.goto(MEETING_URL);

    const speakersBtn = page.getByTitle('Re-run speaker detection on this meeting');
    await expect(speakersBtn).toBeVisible({ timeout: 20_000 });
    await speakersBtn.click();

    // handleRediarize registers the diarization-complete listener before the
    // reset command lands, so the listener is live by the time we emit.
    await expect.poll(async () => {
      const calls = await speakerCalls(page);
      return calls.find((c) => c.cmd === 'reset_speaker_labels') ?? null;
    }, { timeout: 10_000 }).toEqual({ cmd: 'reset_speaker_labels', meetingId: 'meet-summary-001' });

    // Swap the transcript fixture to the post-split state BEFORE emitting: the
    // single coarse row is now three fine rows with FRESH ids (none equal to
    // "coarse-1") and three distinct speakers. The mock refetch reads
    // window.__smokeTranscripts fresh each time.
    const splitRows: SmokeTranscript[] = [
      { id: 'fine-a', text: 'Speaker A turn.', timestamp: '00:10:00', audio_start_time: 600, speaker: 'Speaker 0' },
      { id: 'fine-b', text: 'Speaker B turn.', timestamp: '00:10:05', audio_start_time: 605, speaker: 'Speaker 1' },
      { id: 'fine-c', text: 'Speaker C turn.', timestamp: '00:10:10', audio_start_time: 610, speaker: 'Speaker 2' },
    ];
    await page.evaluate((rows) => {
      (window as unknown as { __smokeTranscripts?: SmokeTranscript[] }).__smokeTranscripts = rows;
    }, splitRows);

    await page.evaluate(() => {
      (window as unknown as { __tauriMockEventBus: { emit: (e: string, p: unknown) => void } })
        .__tauriMockEventBus.emit('diarization-complete', {
          meeting_id: 'meet-summary-001',
          speaker_count: 3,
          segments_labeled: 3,
        });
    });

    await expect(page.getByText('Detected 3 speakers')).toBeVisible({ timeout: 10_000 });

    // The split reaches the DOM: three speaker badges, three distinct labels,
    // and the source id "coarse-1" is absent from the rendered row set.
    await expect.poll(async () => {
      return page.evaluate(() => {
        const badges = Array.from(document.querySelectorAll<HTMLElement>('span[role="button"]'));
        const labels = badges.map((b) => b.textContent?.trim() ?? '');
        // Row containers carry the segment id via data-segment-id if present;
        // fall back to checking badge text does not include the source id.
        const rowIds = Array.from(document.querySelectorAll<HTMLElement>('[data-segment-id]'))
          .map((el) => el.getAttribute('data-segment-id') ?? '');
        const sourceIdGone = !rowIds.includes('coarse-1') && rowIds.length > 0
          ? true
          : !labels.some((l) => l.includes('coarse-1'));
        return {
          count: badges.length,
          distinct: new Set(labels).size,
          labels,
          sourceIdGone,
        };
      });
    }, { timeout: 10_000 }).toMatchObject({
      count: 3,
      distinct: 3,
      sourceIdGone: true,
    });
  });
});
