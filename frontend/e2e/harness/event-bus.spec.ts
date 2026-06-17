import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';

// Task 2.6 — verifies the @tauri-apps/api/event module-seam mock drives
// event-dependent UI from fixtures. The app's `listen` import routes through
// the mock to window.__tauriMockEventBus (installed by the init script); tests
// inject events by calling the mock's `emit`. This is what lets a smoke spec
// simulate 'transcript-update' / recording-state flows without a Rust runtime.

test.describe('event-bus mock (2.6)', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriMockEventActive?: boolean }).__tauriMockEventActive === true,
      { timeout: 15_000 },
    );
  });

  test('listen subscribes and emit delivers the payload as a TauriEvent-shaped object', async ({ page }) => {
    const captured = await page.evaluate(async () => {
      const w = window as unknown as {
        __tauriMockListen: (event: string, handler: (e: unknown) => void) => Promise<() => void>;
        __tauriMockEmit: (event: string, payload?: unknown) => Promise<void>;
      };
      let received: unknown = null;
      await w.__tauriMockListen('transcript-update', (e) => {
        received = e;
      });
      await w.__tauriMockEmit('transcript-update', { text: 'hello world', ts: 1234 });
      return received;
    });

    expect(captured).toEqual({
      event: 'transcript-update',
      id: 0,
      payload: { text: 'hello world', ts: 1234 },
    });
  });

  test('the UnlistenFn returned by listen stops further delivery', async ({ page }) => {
    const result = await page.evaluate(async () => {
      const w = window as unknown as {
        __tauriMockListen: (event: string, handler: (e: unknown) => void) => Promise<() => void>;
        __tauriMockEmit: (event: string, payload?: unknown) => Promise<void>;
      };
      let callCount = 0;
      const unlisten = await w.__tauriMockListen('tick', () => {
        callCount++;
      });
      await w.__tauriMockEmit('tick');
      unlisten();
      await w.__tauriMockEmit('tick');
      await w.__tauriMockEmit('tick');
      return callCount;
    });

    expect(result).toBe(1);
  });
});
