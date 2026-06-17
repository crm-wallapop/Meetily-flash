// Replaces @tauri-apps/api/event when PLAYWRIGHT_E2E=1 (see next.config.js).
// listen/emit/once delegate to window.__tauriMockEventBus, installed by the
// Playwright init script. Tests drive event-driven UI by calling
// eventBus.emit('transcript-update', ...).

export type UnlistenFn = () => void;

export interface TauriEvent<T = unknown> {
  event: string;
  id: number;
  payload: T;
}

interface MockEventBus {
  subscribe(event: string, fn: (e: unknown) => void): number;
  unsubscribe(id: number): void;
  emit(event: string, payload: unknown): void;
}

function getEventBus(): MockEventBus | undefined {
  const w = window as unknown as { __tauriMockEventBus?: MockEventBus };
  return w.__tauriMockEventBus;
}

export async function listen<T = unknown>(
  event: string,
  handler: (e: TauriEvent<T>) => void,
): Promise<UnlistenFn> {
  const bus = getEventBus();
  if (!bus) return () => {};
  const id = bus.subscribe(event, handler as (e: unknown) => void);
  return () => bus.unsubscribe(id);
}

export async function once<T = unknown>(
  event: string,
  handler: (e: TauriEvent<T>) => void,
): Promise<UnlistenFn> {
  const bus = getEventBus();
  if (!bus) return () => {};
  const id = bus.subscribe(event, (e) => {
    bus.unsubscribe(id);
    handler(e as TauriEvent<T>);
  });
  return () => bus.unsubscribe(id);
}

export async function emit(event: string, payload?: unknown): Promise<void> {
  const bus = getEventBus();
  if (bus) bus.emit(event, payload);
}

export async function emitTo(
  _target: string,
  event: string,
  payload?: unknown,
): Promise<void> {
  // Single-page test environment has no other webviews to target; route locally.
  const bus = getEventBus();
  if (bus) bus.emit(event, payload);
}

// Proof-of-life markers for the event-bus test (task 2.6). The real
// @tauri-apps/api/event never sets these. Guarded for SSR.
if (typeof window !== 'undefined') {
  const w = window as unknown as {
    __tauriMockEventActive?: boolean;
    __tauriMockListen?: typeof listen;
    __tauriMockEmit?: typeof emit;
  };
  w.__tauriMockEventActive = true;
  w.__tauriMockListen = listen;
  w.__tauriMockEmit = emit;
}
