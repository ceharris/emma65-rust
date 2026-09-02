/**
 * Shared Tauri IPC mock for Vitest, covering the four `@tauri-apps/api`
 * entry points the frontend actually imports (`core`'s `invoke`, `event`'s
 * `listen`/`emitTo`, `window`'s `getCurrentWindow`). `vi.mock` calls must
 * live in a module reachable from `setupFiles` (see `vite.config.ts`) so
 * they're registered before any test file imports the real modules — see
 * https://vitest.dev/api/vi.html#vi-mock for the hoisting contract this
 * relies on.
 *
 * Usage: call `resetTauriMocks()` in a `beforeEach`, then configure
 * `invoke`/`getCurrentWindow`'s return values per test as needed
 * (`invoke.mockResolvedValueOnce(...)`, `getCurrentWindow.mockReturnValue(...)`).
 * Use `emitMockEvent(event, payload)` to simulate a backend-pushed event
 * reaching whatever handler(s) a test registered via `listen`.
 */
import { vi } from "vitest";

interface MockEvent<T> {
  event: string;
  id: number;
  payload: T;
}

type MockEventHandler = (event: MockEvent<unknown>) => void;

const {
  invoke,
  listen,
  emitTo,
  getCurrentWindow,
  listenersByEvent,
  nextEventId,
} = vi.hoisted(() => {
  return {
    invoke: vi.fn(),
    listen: vi.fn((event: string, handler: MockEventHandler) => {
      let handlers = listenersByEvent.get(event);
      if (!handlers) {
        handlers = new Set();
        listenersByEvent.set(event, handlers);
      }
      handlers.add(handler);
      return Promise.resolve(() => {
        handlers?.delete(handler);
      });
    }),
    emitTo: vi.fn(() => Promise.resolve()),
    getCurrentWindow: vi.fn(() => ({ label: "main" })),
    listenersByEvent: new Map<string, Set<MockEventHandler>>(),
    nextEventId: { current: 0 },
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen, emitTo }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow }));

export { invoke, listen, emitTo, getCurrentWindow };

/** Dispatches `payload` to every handler currently registered for `event` via the mocked `listen`. */
export function emitMockEvent<T>(event: string, payload: T): void {
  const handlers = listenersByEvent.get(event);
  if (!handlers) return;
  const mockEvent: MockEvent<T> = { event, id: nextEventId.current++, payload };
  for (const handler of handlers) handler(mockEvent as MockEvent<unknown>);
}

/**
 * Clears call history and restores default behavior for all four mocks, and
 * drops every `listen` registration. Call in a `beforeEach` so tests don't
 * leak `invoke` return values or event listeners into each other.
 */
export function resetTauriMocks(): void {
  invoke.mockReset();
  emitTo.mockClear();
  emitTo.mockResolvedValue(undefined);
  getCurrentWindow.mockClear();
  getCurrentWindow.mockReturnValue({ label: "main" });
  listen.mockClear();
  listenersByEvent.clear();
  nextEventId.current = 0;
}
