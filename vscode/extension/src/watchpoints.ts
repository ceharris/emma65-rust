import * as vscode from 'vscode';
import * as path from 'path';

/**
 * Custom `evaluate` contexts the adapter recognizes as watchpoint commands — see
 * `vscode/adapter/src/watchpoints.rs`'s module doc comment for why `evaluate` is the
 * escape hatch used instead of a genuinely custom DAP request (same `dap`-crate
 * limitation `bus.ts`/`trace.ts` already work around for stories 9/10). Like
 * `trace.ts`, these carry JSON in both `expression` (request payload) and `result`
 * (response payload), since a watchpoints snapshot is structured data.
 */
const GET_WATCHPOINTS = 'emma65.getWatchpoints';
const ADD_WATCHPOINT = 'emma65.addWatchpoint';
const REMOVE_WATCHPOINT = 'emma65.removeWatchpoint';
const EDIT_WATCHPOINT = 'emma65.editWatchpoint';
const TOGGLE_WATCHPOINT = 'emma65.toggleWatchpoint';

/** The active `emma65` debug session, or `undefined` if none is active. */
function activeSession(): vscode.DebugSession | undefined {
  const session = vscode.debug.activeDebugSession;
  return session?.type === 'emma65' ? session : undefined;
}

/** Sends a watchpoint `evaluate` context (with a JSON-encoded payload) to the active
 * session and JSON-decodes its result. Throws if there is no active session or the
 * adapter reports an error (e.g. "CPU not ready", a compile error). */
async function callWatchpoints<T>(context: string, payload: unknown = {}): Promise<T> {
  const session = activeSession();
  if (!session) {
    throw new Error('no active debug session');
  }
  const response = await session.customRequest('evaluate', {
    expression: JSON.stringify(payload),
    context,
  });
  return JSON.parse(response.result) as T;
}

let panel: vscode.WebviewPanel | undefined;

/** Opens (or reveals, if already open) the watchpoints webview panel. */
function showWatchpointsPanel(context: vscode.ExtensionContext): void {
  if (panel) {
    panel.reveal();
    return;
  }

  panel = vscode.window.createWebviewPanel('emma65.watchpoints', 'emma65 Watchpoints', vscode.ViewColumn.Beside, {
    enableScripts: true,
    retainContextWhenHidden: true,
    localResourceRoots: [vscode.Uri.file(path.join(context.extensionPath, 'webview-src', 'dist'))],
  });
  panel.webview.html = buildHtml(panel.webview, context);

  panel.webview.onDidReceiveMessage((message) => handleWebviewMessage(panel!, message));
  panel.onDidDispose(() => {
    panel = undefined;
  });
}

/** A request from the webview, correlated to a response by `id`. */
interface WebviewRequest {
  type: 'request';
  id: number;
  command: 'get' | 'add' | 'remove' | 'edit' | 'toggle';
  args?: { source?: string; index?: number };
}

/** Dispatches one webview request to the matching watchpoint command and posts back a
 * `{ type: 'response', id, ok, result | error }` message. */
async function handleWebviewMessage(target: vscode.WebviewPanel, message: WebviewRequest): Promise<void> {
  if (message.type !== 'request') {
    return;
  }
  try {
    const result = await runWatchpointCommand(message.command, message.args);
    target.webview.postMessage({ type: 'response', id: message.id, ok: true, result });
  } catch (err) {
    target.webview.postMessage({
      type: 'response',
      id: message.id,
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    });
  }
}

async function runWatchpointCommand(
  command: WebviewRequest['command'],
  args?: { source?: string; index?: number },
): Promise<unknown> {
  switch (command) {
    case 'get':
      return callWatchpoints(GET_WATCHPOINTS);
    case 'add':
      return callWatchpoints(ADD_WATCHPOINT, { source: args?.source });
    case 'remove':
      return callWatchpoints(REMOVE_WATCHPOINT, { index: args?.index });
    case 'edit':
      return callWatchpoints(EDIT_WATCHPOINT, { index: args?.index, source: args?.source });
    case 'toggle':
      return callWatchpoints(TOGGLE_WATCHPOINT, { index: args?.index });
  }
}

/** Builds the webview's HTML shell, loading the Vite-built bundle from
 * `webview-src/dist` — or, when `EMMA65_WATCHPOINTS_WEBVIEW_DEV=1` is set in the
 * extension host's environment, from the local Vite dev server for hot-reload
 * iteration (same dev server `trace.ts` points at, just a different entry module). */
function buildHtml(webview: vscode.Webview, context: vscode.ExtensionContext): string {
  const nonce = getNonce();

  if (process.env.EMMA65_WATCHPOINTS_WEBVIEW_DEV === '1') {
    const devServer = 'http://localhost:5174';
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src ${devServer} 'unsafe-inline'; style-src ${devServer} 'unsafe-inline'; connect-src ${devServer} ws://localhost:5174; font-src ${devServer};" />
  <title>emma65 Watchpoints</title>
</head>
<body>
  <div id="root"></div>
  <script type="module" src="${devServer}/@vite/client"></script>
  <script type="module" src="${devServer}/src/watchpoints.tsx"></script>
</body>
</html>`;
  }

  const distDir = vscode.Uri.file(path.join(context.extensionPath, 'webview-src', 'dist'));
  const scriptUri = webview.asWebviewUri(vscode.Uri.joinPath(distDir, 'watchpoints.js'));
  const styleUri = webview.asWebviewUri(vscode.Uri.joinPath(distDir, 'watchpoints.css'));

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'nonce-${nonce}'; style-src ${webview.cspSource} 'unsafe-inline'; font-src ${webview.cspSource};" />
  <link rel="stylesheet" href="${styleUri}" />
  <title>emma65 Watchpoints</title>
</head>
<body>
  <div id="root"></div>
  <script nonce="${nonce}" type="module" src="${scriptUri}"></script>
</body>
</html>`;
}

function getNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let nonce = '';
  for (let i = 0; i < 32; i++) {
    nonce += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return nonce;
}

/**
 * Logs `stopped` DAP events to the watchpoints panel as a `halted` push event,
 * driving its re-evaluate-on-halt behavior — the same live-follow pattern
 * `trace.ts`'s `TraceLiveFollowTrackerFactory` established for story 10. Registered
 * as its own `DebugAdapterTrackerFactory` alongside the other two; VS Code invokes
 * every registered factory for a given session, so this doesn't interfere with them.
 */
class WatchpointsLiveFollowTrackerFactory implements vscode.DebugAdapterTrackerFactory {
  createDebugAdapterTracker(_session: vscode.DebugSession): vscode.ProviderResult<vscode.DebugAdapterTracker> {
    return {
      onDidSendMessage(message: unknown) {
        const event = message as { type?: string; event?: string };
        if (event.type === 'event' && event.event === 'stopped') {
          panel?.webview.postMessage({ type: 'event', event: 'halted' });
        }
      },
    };
  }
}

/** Registers the watchpoints command and its DAP-event-driven live-follow tracker. */
export function registerWatchpoints(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('emma65.showWatchpoints', () => showWatchpointsPanel(context)),
    vscode.debug.registerDebugAdapterTrackerFactory('emma65', new WatchpointsLiveFollowTrackerFactory()),
  );
}
