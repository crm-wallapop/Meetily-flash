import { test, expect } from '@playwright/test';
import { bootstrap, speakerCalls, type SmokeSpeaker } from './_speaker-helpers';

// Smoke spec for the speaker-rename-cancel change: the inline SpeakerLabelInput must
// cancel (not commit) when focus leaves it, while suggestion-chip clicks must still
// submit. Proves the onBlur + onMouseDown wiring in SpeakerBadge.tsx. The pre-push
// hook derives this filename from the `fix/speaker-rename-cancel` branch.

const NAMED_SPEAKERS: SmokeSpeaker[] = [
  { id: 's1', name: 'Alice', color: 'hsl(137, 65%, 55%)' },
];

test.describe('speaker-rename-cancel smoke', () => {
  test.beforeEach(() => {
    // Cold first-compile of /meeting-details is slow; match the sibling spec's budget.
    test.setTimeout(120_000);
  });

  test('1.1 — blur cancels the inline input (click-outside and Tab) without dispatching label_speaker', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
    ]);

    const badge = page.getByRole('button', { name: 'Speaker 0' });
    await expect(badge).toBeVisible({ timeout: 20_000 });

    // Click outside the input (the summary section heading) moves focus away → blur cancels.
    await badge.click();
    const input = page.getByPlaceholder('Enter speaker name...');
    await expect(input).toBeVisible({ timeout: 5_000 });
    await page.getByText('Key Decisions').click();
    await expect(input).toBeHidden({ timeout: 5_000 });

    // Tab moves focus to the scope checkbox (inside the container, so the input
    // stays open); a second Tab leaves the container and the blur cancels.
    await badge.click();
    const inputTab = page.getByPlaceholder('Enter speaker name...');
    await expect(inputTab).toBeVisible({ timeout: 5_000 });
    await inputTab.press('Tab');
    await expect(inputTab).toBeVisible({ timeout: 5_000 });
    await page.getByRole('checkbox', { name: /Also rename all/ }).press('Tab');
    await expect(inputTab).toBeHidden({ timeout: 5_000 });

    const calls = await speakerCalls(page);
    expect(calls.find((c) => c.cmd === 'label_speaker'), 'no label_speaker on cancel').toBeUndefined();
  });

  test('1.2 — typed name is discarded on click-outside (no accidental commit)', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
    ]);

    const badge = page.getByRole('button', { name: 'Speaker 0' });
    await expect(badge).toBeVisible({ timeout: 20_000 });
    await badge.click();

    const input = page.getByPlaceholder('Enter speaker name...');
    await expect(input).toBeVisible({ timeout: 5_000 });
    await input.fill('Alice');

    // Click outside — the typed name must be discarded, NOT committed.
    await page.getByText('Key Decisions').click();

    await expect(input).toBeHidden({ timeout: 5_000 });
    const calls = await speakerCalls(page);
    expect(calls.find((c) => c.cmd === 'label_speaker'), 'typed name must not be committed on blur').toBeUndefined();
  });

  test('1.3 — suggestion chip still submits after the blur guard', async ({ page }) => {
    await bootstrap(
      page,
      [{ id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' }],
      NAMED_SPEAKERS,
    );

    const badge = page.getByRole('button', { name: 'Speaker 0' });
    await expect(badge).toBeVisible({ timeout: 20_000 });
    await badge.click();

    const input = page.getByPlaceholder('Enter speaker name...');
    await expect(input).toBeVisible({ timeout: 5_000 });
    // Type a prefix matching "Alice" so the chip is filtered into view.
    await input.fill('A');

    const chip = page.getByRole('button', { name: 'Alice' });
    await expect(chip).toBeVisible({ timeout: 5_000 });
    await chip.click();

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

  test('1.5 — Escape cancels and Enter submits (keyboard paths unchanged)', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
    ]);

    // Escape cancels.
    let badge = page.getByRole('button', { name: 'Speaker 0' });
    await expect(badge).toBeVisible({ timeout: 20_000 });
    await badge.click();

    const input = page.getByPlaceholder('Enter speaker name...');
    await expect(input).toBeVisible({ timeout: 5_000 });
    await input.press('Escape');
    await expect(input).toBeHidden({ timeout: 5_000 });
    const calls = await speakerCalls(page);
    expect(calls.find((c) => c.cmd === 'label_speaker'), 'Escape must not submit').toBeUndefined();

    // Enter submits.
    badge = page.getByRole('button', { name: 'Speaker 0' });
    await badge.click();
    const input2 = page.getByPlaceholder('Enter speaker name...');
    await expect(input2).toBeVisible({ timeout: 5_000 });
    await input2.fill('Bob');
    await input2.press('Enter');

    await expect.poll(async () => {
      const c = await speakerCalls(page);
      return c.find((x) => x.cmd === 'label_speaker') ?? null;
    }, { timeout: 10_000 }).toMatchObject({ cmd: 'label_speaker', speakerName: 'Bob' });
  });
});
