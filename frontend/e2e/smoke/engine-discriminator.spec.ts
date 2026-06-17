import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';

// Task 4.2 — proves the engine-per-OS channel selection actually discriminates.
// Sets `-webkit-hyphens: auto` and reads back the computed value. Chromium
// normalizes to the unprefixed `hyphens` and does NOT echo the `-webkit-` prefixed
// property (computed: ''). WebKit still recognizes its prefixed form (computed:
// 'auto'). The two branches assert DIFFERENT values; if the channel selection
// broke (e.g. both projects ran the same engine), one branch would execute against
// the wrong engine and fail. That is the proof.
//
// Candidate properties were probed on BOTH engines via a direct Playwright launch
// (see commit history). -webkit-text-security, -webkit-overflow-scrolling, and
// -webkit-marquee-style all converged to identical computed values across engines
// as of Playwright webkit v2311 / chromium v1228. -webkit-hyphens is the lone
// survivor that still discriminates deterministically. navigator.vendor
// ('Apple Computer, Inc.' vs 'Google Inc.') is a JS-level backup signal but the
// spec asks for a CSS feature, so the computed-style check is primary.

test.describe('engine-discriminator smoke (4.2)', () => {
  test('-webkit-hyphens computed value differs by engine family', async ({ page, browserName }) => {
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    const computed = await page.evaluate(() => {
      const el = document.createElement('span');
      el.style.setProperty('-webkit-hyphens', 'auto');
      el.textContent = 'probe';
      document.body.appendChild(el);
      return getComputedStyle(el).getPropertyValue('-webkit-hyphens').trim();
    });

    if (browserName === 'chromium') {
      expect(computed, 'chromium must not echo the -webkit-hyphens prefix').toBe('');
    } else {
      expect(computed, 'webkit must honor -webkit-hyphens').toBe('auto');
    }
  });
});
