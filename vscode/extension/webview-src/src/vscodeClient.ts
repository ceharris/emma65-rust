/**
 * Minimal request/response + push-event client over `acquireVsCodeApi().postMessage`,
 * mirroring the shape of Tauri's `invoke`/`listen` closely enough that `TracePanel.tsx`
 * (ported from `debugger/frontend/src/TracePanel.tsx`) barely had to change its call
 * sites. The extension host side of this protocol lives in
 * `vscode/extension/src/trace.ts`.
 */

interface VsCodeApi {
  postMessage(message: unknown): void;
}

declare function acquireVsCodeApi(): VsCodeApi;

const vscode = acquireVsCodeApi();

interface RequestMessage {
  type: "request";
  id: number;
  command: string;
  args?: unknown;
}

type IncomingMessage =
  | { type: "response"; id: number; ok: true; result: unknown }
  | { type: "response"; id: number; ok: false; error: string }
  | { type: "event"; event: string; payload?: unknown };

let nextRequestId = 1;
const pending = new Map<number, { resolve: (value: unknown) => void; reject: (reason: unknown) => void }>();
const eventListeners = new Map<string, Set<(payload: unknown) => void>>();

window.addEventListener("message", (e: MessageEvent<IncomingMessage>) => {
  const message = e.data;
  if (message.type === "response") {
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.ok) request.resolve(message.result);
    else request.reject(new Error(message.error));
  } else if (message.type === "event") {
    eventListeners.get(message.event)?.forEach((listener) => listener(message.payload));
  }
});

/** Sends `command` to the extension host and resolves with its result. */
export function invoke<T>(command: string, args?: unknown): Promise<T> {
  return new Promise((resolve, reject) => {
    const id = nextRequestId++;
    pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
    const message: RequestMessage = { type: "request", id, command, args };
    vscode.postMessage(message);
  });
}

/** Subscribes to a push event from the extension host. Returns an unsubscribe function. */
export function listen(event: string, handler: (payload: unknown) => void): () => void {
  let listeners = eventListeners.get(event);
  if (!listeners) {
    listeners = new Set();
    eventListeners.set(event, listeners);
  }
  listeners.add(handler);
  return () => listeners.delete(handler);
}
