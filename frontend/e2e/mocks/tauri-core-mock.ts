// Replaces @tauri-apps/api/core when PLAYWRIGHT_E2E=1 (see next.config.js).
//
// Only `invoke` is actually driven by tests. The app imports nothing else from
// this module, but the bundled Tauri plugins (notification, fs, store, updater)
// statically import Resource, Channel, and addPluginListener — without these
// exports webpack fails with "export 'X' was not found" and every page returns
// HTTP 500. The stubs below exist solely so the plugin modules bundle; they are
// never exercised by a UI smoke test, which drives the app through invoke + the
// event bus only.
//
// invoke delegates to window.__tauriMockDispatcher, installed by the Playwright
// init script (e2e/mocks/init-script.ts). The real invoke reaches into
// window.__TAURI_INTERNALS__; this mock does not — that is the spec's D2 point
// and what the module-seam test (task 2.3) asserts.

export class Resource {
  rid: number;
  constructor(rid: number) {
    this.rid = rid;
  }
}

export class Channel<T = unknown> {
  onmessage?: (response: T) => void;
  setup(onmessage: (response: T) => void): number {
    this.onmessage = onmessage;
    return 0;
  }
  emit(response: T): void {
    this.onmessage?.(response);
  }
  toJSON(): { __tauriModule__: string; channel: number; id: number } {
    return { __tauriModule__: 'Event', channel: 0, id: 0 };
  }
}

export interface PluginListenerEvent<T = unknown> {
  event: string;
  id: number;
  payload: T;
}

export async function addPluginListener(
  _event: string,
  _handler: (event: PluginListenerEvent) => void,
): Promise<() => void> {
  return () => {};
}

type MockHandler = (args: unknown) => unknown | Promise<unknown>;

interface MockDispatcher {
  (cmd: string, args?: Record<string, unknown>): Promise<unknown>;
  register(cmd: string, handler: MockHandler): void;
  registerMany(map: Record<string, MockHandler>): void;
  registeredCommands(): string[];
}

function getDispatcher(): MockDispatcher {
  const w = window as unknown as { __tauriMockDispatcher?: MockDispatcher };
  if (!w.__tauriMockDispatcher) {
    throw new Error(
      '[tauri-mock] No dispatcher registered. The Playwright init script did not run — ' +
        'is PLAYWRIGHT_E2E set and the webpack alias active?',
    );
  }
  return w.__tauriMockDispatcher;
}

export async function invoke<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return (await getDispatcher()(cmd, args)) as T;
}

// Proof-of-life markers for the module-seam test (task 2.3). The real
// @tauri-apps/api/core never sets these; if they exist, the webpack alias is
// active and the app's `invoke` import resolved to this mock. Exposed on window
// because page.evaluate cannot reach a webpack module export directly.
// Guarded for SSR — Next.js evaluates this module on the server too.
if (typeof window !== 'undefined') {
  const w = window as unknown as {
    __tauriCoreMockActive?: boolean;
    __tauriMockInvoke?: typeof invoke;
  };
  w.__tauriCoreMockActive = true;
  w.__tauriMockInvoke = invoke;
}
