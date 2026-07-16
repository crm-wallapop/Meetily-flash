import { test, expect } from '@playwright/test';
import { bootstrap, speakerCalls } from './_speaker-helpers';

// Smoke for the diarization-label-quality change. The two-pass re-labeling,
// token-timestamp alignment, and temporal-presence orphan scan are all Rust-side
// label-quality improvements locked by cargo tests (§3/§4/§5). No new Tauri
// commands or events are introduced — the change is observable in the UI only
// through what the rediarize refetch now returns: a single multi-speaker
// transcript window that previously carried one label renders as multiple
// per-speaker rows after re-detection. This spec pins that event→refetch→render
// wiring (per feedback_smoke_carveout: assert the wiring, not the backend logic).

test.describe('diarization-label-quality smoke', () => {
  test.beforeEach(() => {
    // Cold first-compile of /meeting-details is slow (see summary-render smoke);
    // the default 30s budget aborts page.goto mid-compile.
    test.setTimeout(120_000);
  });

  test('per-speaker segment splits render after rediarize (single multi-speaker window → two labeled rows)', async ({ page }) => {
    // Bootstrap with ONE long window spanning two speakers under a single label —
    // the pre-fix state where the second speaker's interjection is swallowed.
    await bootstrap(page, [
      { id: 't1', text: 'Long window spanning two speakers.', timestamp: '00:46:42', audio_start_time: 2802, speaker: 'Speaker 0' },
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

    // Swap the transcript fixture to the post-diarization split BEFORE emitting
    // the completion event: the single multi-speaker window is now two
    // per-speaker rows (the two-pass + token-alignment result the Rust backend
    // would produce and write back). The mock handler reads
    // window.__smokeTranscripts fresh on each fetch, so the next refetch serves this.
    await page.evaluate(() => {
      (window as unknown as { __smokeTranscripts?: unknown[] }).__smokeTranscripts = [
        { id: 't1', text: 'First speaker turn.', timestamp: '00:46:42', audio_start_time: 2802, speaker: 'Speaker 0' },
        { id: 't2', text: 'Second speaker interjection.', timestamp: '00:46:58', audio_start_time: 2818, speaker: 'Speaker 1' },
      ];
    });

    await page.evaluate(() => {
      (window as unknown as { __tauriMockEventBus: { emit: (e: string, p: unknown) => void } })
        .__tauriMockEventBus.emit('diarization-complete', {
          meeting_id: 'meet-summary-001',
          speaker_count: 2,
          segments_labeled: 2,
        });
    });

    // The completion handler toasts the detected count.
    await expect(page.getByText('Detected 2 speakers')).toBeVisible({ timeout: 10_000 });

    // The handler calls onRefetchTranscripts on completion — the fetch counter
    // must tick a second time, proving the re-diarized transcripts are reloaded.
    await expect.poll(async () => {
      return page.evaluate(() =>
        (window as unknown as { __smokeTranscriptsFetchCount?: number }).__smokeTranscriptsFetchCount ?? 0,
      );
    }, { timeout: 10_000 }).toBeGreaterThanOrEqual(2);

    // The split reaches the DOM: two speaker badges render with distinct labels.
    // This is the user-visible deliverable — a window that was one label now
    // renders as two per-speaker rows.
    await expect.poll(async () => {
      return page.evaluate(() => {
        const badges = Array.from(document.querySelectorAll<HTMLElement>('span[role="button"]'));
        const labels = badges.map((b) => b.textContent?.trim() ?? '');
        return { count: badges.length, distinct: new Set(labels).size, labels };
      });
    }, { timeout: 10_000 }).toMatchObject({ count: 2, distinct: 2 });
  });
});
