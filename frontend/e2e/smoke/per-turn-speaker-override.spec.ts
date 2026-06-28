import { test, expect } from '@playwright/test';
import { bootstrap, speakerCalls, type SmokeSpeaker } from './_speaker-helpers';

// Smoke spec for the per-turn-speaker-override change: the inline SpeakerLabelInput
// gains a scope checkbox — checked (default) = cluster rename = today's behavior;
// unchecked = single-segment override via set_segment_speaker. Proves the scope
// toggle branches the dispatch and that chips respect it. The pre-push hook derives
// this filename from the `enhance/per-turn-speaker-override` branch.

const NAMED_SPEAKERS: SmokeSpeaker[] = [
  { id: 's1', name: 'Alice', color: 'hsl(137, 65%, 55%)' },
];

test.describe('per-turn-speaker-override smoke', () => {
  test.beforeEach(() => {
    // Cold first-compile of /meeting-details is slow; match the sibling spec's budget.
    test.setTimeout(120_000);
  });

  test('1.1 — scope checkbox visible, checked by default, labeled with the cluster label', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
    ]);

    const badge = page.getByRole('button', { name: 'Speaker 0' });
    await expect(badge).toBeVisible({ timeout: 20_000 });
    await badge.click();

    const input = page.getByPlaceholder('Enter speaker name...');
    await expect(input).toBeVisible({ timeout: 5_000 });

    const scopeCheckbox = page.getByRole('checkbox', { name: /Also rename all/ });
    await expect(scopeCheckbox).toBeVisible({ timeout: 5_000 });
    // Default is cluster (checked) so the pre-existing rename flow is preserved.
    await expect(scopeCheckbox).toBeChecked();
    // The label names the cluster so the consequence is visible before submit.
    await expect(page.getByText(/Also rename all 'Speaker 0' segments/)).toBeVisible();
  });

  test('1.2 — cluster default (checkbox checked) dispatches label_speaker, not set_segment_speaker', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
    ]);

    const badge = page.getByRole('button', { name: 'Speaker 0' });
    await expect(badge).toBeVisible({ timeout: 20_000 });
    await badge.click();

    const input = page.getByPlaceholder('Enter speaker name...');
    await expect(input).toBeVisible({ timeout: 5_000 });
    // Checkbox defaults to checked (cluster). Type + Enter without touching the box.
    await input.fill('Alice');
    await input.press('Enter');

    await expect.poll(async () => {
      const calls = await speakerCalls(page);
      return calls.find((c) => c.cmd === 'label_speaker') ?? null;
    }, { timeout: 10_000 }).toEqual({
      cmd: 'label_speaker',
      meetingId: 'meet-summary-001',
      clusterLabel: 'Speaker 0',
      speakerName: 'Alice',
    });
    const calls = await speakerCalls(page);
    expect(calls.find((c) => c.cmd === 'set_segment_speaker'), 'set_segment_speaker must NOT fire in cluster mode').toBeUndefined();
  });

  test('1.3 + 2.1 — segment mode (checkbox unchecked) dispatches set_segment_speaker with transcriptId, not label_speaker', async ({ page }) => {
    await bootstrap(page, [
      { id: 't1', text: 'Speaker zero talking.', timestamp: '00:00:01', audio_start_time: 0, speaker: 'Speaker 0' },
    ]);

    const badge = page.getByRole('button', { name: 'Speaker 0' });
    await expect(badge).toBeVisible({ timeout: 20_000 });
    await badge.click();

    const input = page.getByPlaceholder('Enter speaker name...');
    await expect(input).toBeVisible({ timeout: 5_000 });

    // Uncheck → segment scope.
    const scopeCheckbox = page.getByRole('checkbox', { name: /Also rename all/ });
    await scopeCheckbox.uncheck();
    await input.fill('Carlos');
    await input.press('Enter');

    await expect.poll(async () => {
      const calls = await speakerCalls(page);
      return calls.find((c) => c.cmd === 'set_segment_speaker') ?? null;
    }, { timeout: 10_000 }).toEqual({
      cmd: 'set_segment_speaker',
      transcriptId: 't1',
      speakerLabel: 'Carlos',
    });
    const calls = await speakerCalls(page);
    expect(calls.find((c) => c.cmd === 'label_speaker'), 'label_speaker must NOT fire in segment mode').toBeUndefined();
  });

  test('1.4 — suggestion chip respects segment scope (uncheck, then click chip)', async ({ page }) => {
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

    const scopeCheckbox = page.getByRole('checkbox', { name: /Also rename all/ });
    await scopeCheckbox.uncheck();
    // Type a prefix matching "Alice" so the chip is filtered into view.
    await input.fill('A');

    const chip = page.getByRole('button', { name: 'Alice' });
    await expect(chip).toBeVisible({ timeout: 5_000 });
    await chip.click();

    await expect.poll(async () => {
      const calls = await speakerCalls(page);
      return calls.find((c) => c.cmd === 'set_segment_speaker') ?? null;
    }, { timeout: 10_000 }).toEqual({
      cmd: 'set_segment_speaker',
      transcriptId: 't1',
      speakerLabel: 'Alice',
    });
    const calls = await speakerCalls(page);
    expect(calls.find((c) => c.cmd === 'label_speaker'), 'chip in segment mode must NOT fire label_speaker').toBeUndefined();
  });
});
