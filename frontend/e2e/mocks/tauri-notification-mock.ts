// Module-seam mock for @tauri-apps/plugin-notification. The app imports only onAction
// (layout.tsx), which in the real runtime returns an unsubscribe handle the layout
// useEffect treats as { unregister }. There is no notification plugin in the browser
// test runtime, so without this alias onAction resolves to a handle without unregister
// and the effect cleanup throws "listener.unregister is not a function" — surfacing as
// a Next.js error overlay that covers the page under test.
export async function onAction(
  _cb: (...args: unknown[]) => unknown | Promise<unknown>,
): Promise<{ unregister: () => Promise<void> }> {
  return { unregister: () => Promise.resolve() };
}
