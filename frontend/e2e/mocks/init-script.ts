// Self-contained IIFE injected via page.addInitScript before any app script.
// Installs window.__tauriMockDispatcher (fail-closed, fixture-backed) and
// window.__tauriMockEventBus (subscribe/unsubscribe/emit for event-driven flows).
//
// Must be plain JS (no TypeScript annotations) — Playwright evaluates the string
// verbatim in the page context.

export const TAURI_MOCK_INIT_SCRIPT = `
(function () {
  'use strict';
  var handlers = Object.create(null);
  var callLog = [];

  var dispatcher = async function (cmd, args) {
    callLog.push(cmd);
    var handler = handlers[cmd];
    if (typeof handler !== 'function') {
      throw new Error('[tauri-mock] Unregistered command: ' + cmd);
    }
    return await handler(args || {});
  };
  dispatcher.register = function (cmd, handler) {
    handlers[cmd] = handler;
  };
  dispatcher.registerMany = function (map) {
    for (var k in map) {
      if (Object.prototype.hasOwnProperty.call(map, k)) handlers[k] = map[k];
    }
  };
  dispatcher.registeredCommands = function () {
    return Object.keys(handlers);
  };
  dispatcher.callLog = function () {
    return callLog.slice();
  };
  Object.defineProperty(window, '__tauriMockDispatcher', {
    value: dispatcher,
    writable: false,
    configurable: false,
  });

  var listeners = new Map();
  var nextId = 1;
  var eventBus = {
    subscribe: function (event, fn) {
      var id = nextId++;
      if (!listeners.has(event)) listeners.set(event, []);
      listeners.get(event).push({ id: id, fn: fn });
      return id;
    },
    unsubscribe: function (id) {
      listeners.forEach(function (arr, event) {
        listeners.set(
          event,
          arr.filter(function (l) { return l.id !== id; })
        );
      });
    },
    emit: function (event, payload) {
      var arr = listeners.get(event) || [];
      arr.forEach(function (l) { l.fn({ event: event, payload: payload, id: 0 }); });
    },
    // Read-only probe so a spec can wait until the UI has subscribed before
    // emitting — the dialog registers its completion listener asynchronously
    // on open, and emitting before subscription silently drops the event.
    listenerCount: function (event) {
      return (listeners.get(event) || []).length;
    },
  };
  Object.defineProperty(window, '__tauriMockEventBus', {
    value: eventBus,
    writable: false,
    configurable: false,
  });
})();
`;

// Install a spy on window.__TAURI_INTERNALS__ that counts accesses, with a
// reset hook. Used by the module-seam test (task 2.3) to prove the mock never
// reaches into Tauri internals. Plugins loaded by the app may legitimately
// touch __TAURI_INTERNALS__ during page load; tests call reset() immediately
// before the action under test to isolate the assertion to that interaction.
export const TAURI_INTERNALS_SPY_INIT_SCRIPT = `
(function () {
  'use strict';
  var count = 0;
  Object.defineProperty(window, '__TAURI_INTERNALS__', {
    get: function () { count++; return undefined; },
    configurable: true,
  });
  window.__tauriInternalsAccessCount = function () { return count; };
  window.__resetTauriInternalsSpy = function () { count = 0; };
})();
`;
