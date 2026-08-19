/**
 * Bridges the native Assembler menu's `assembler-menu-action` Tauri event to
 * `AssemblerPanel.tsx`, without requiring the panel to already be mounted
 * when the event arrives.
 *
 * `on_menu_event` (`lib.rs`) emits `reveal-panel` and `assembler-menu-action`
 * back-to-back, synchronously, whenever an Assembler menu item is clicked.
 * Assembler is the only panel in this app with its own action-dispatching
 * menu that isn't part of the default dock layout (`addDefaultLayout` in
 * `DockLayout.tsx`) — every other such panel (Memory, Run Controls, …) is
 * always mounted from app startup, so its own `listen()`-based menu-action
 * effect is already registered well before any menu click is possible.
 * Assembler's dock tab is instead created lazily on first reveal, which
 * means a `listen("assembler-menu-action", ...)` call made from inside
 * `AssemblerPanel.tsx`'s own mount effect would race against the *already
 * in-flight* `assembler-menu-action` event triggered by that exact same
 * click — an async `listen()` registration can lose that race, and Tauri
 * does not replay a missed event to a listener that subscribes after it was
 * emitted. Confirmed via diagnostic logging (issue #474 debugger
 * integration Unit 4 follow-up, 2026-08-19): the event was reliably lost
 * once added console logging shifted the timing further against the
 * listener — it wasn't an intermittent fluke.
 *
 * Fix: a single `listen("assembler-menu-action", ...)` subscription lives in
 * `DockLayout.tsx`, mounted once at app startup alongside its own
 * `reveal-panel` listener (both are always active well before this race
 * window can open) and calls `dispatchAssemblerMenuAction` below, which
 * either forwards the action synchronously to the currently-registered
 * `AssemblerPanel` handler, or — if the panel isn't mounted yet — remembers
 * it as `pendingAction` and replays it the moment `registerAssemblerPanel`
 * runs. All of this is plain synchronous JS (no further `listen()`/
 * `unlisten()` async round trips), so there's no remaining window for the
 * action to be lost, however React StrictMode's dev-only double-invoke of
 * `AssemblerPanel`'s own mount effect settles.
 */

type AssemblerMenuActionHandler = (action: string) => void;

let currentHandler: AssemblerMenuActionHandler | null = null;
let pendingAction: string | null = null;

/**
 * Called by `AssemblerPanel.tsx`'s own mount effect. Replays a pending
 * action immediately, synchronously, if one arrived before the panel was
 * ready to handle it. Returns an unregister function for the effect's
 * cleanup.
 */
export function registerAssemblerPanel(handler: AssemblerMenuActionHandler): () => void {
  currentHandler = handler;
  if (pendingAction !== null) {
    const action = pendingAction;
    pendingAction = null;
    handler(action);
  }
  return () => {
    if (currentHandler === handler) currentHandler = null;
  };
}

/**
 * Called by `DockLayout.tsx`'s always-active `assembler-menu-action`
 * listener for every event it receives.
 */
export function dispatchAssemblerMenuAction(action: string): void {
  if (currentHandler) {
    currentHandler(action);
  } else {
    pendingAction = action;
  }
}
