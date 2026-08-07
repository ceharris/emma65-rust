import * as vscode from 'vscode';
import * as path from 'path';

/**
 * Custom `evaluate` contexts the adapter recognizes as trace commands — see
 * `vscode/adapter/src/trace.rs`'s module doc comment for why `evaluate` is the escape
 * hatch used instead of a genuinely custom DAP request (same `dap`-crate limitation
 * `bus.ts`/`bus.rs` already work around for story 9). Unlike `bus.ts`'s one-line
 * result strings, these carry JSON in both `expression` (request payload) and
 * `result` (response payload), since trace rows/pages are structured data.
 */
const RECORD_TRACE = 'emma65.recordTrace';
const STOP_TRACE = 'emma65.stopTrace';
const GET_TRACE_WINDOW = 'emma65.getTraceWindow';
const GET_TRACE_STATUS = 'emma65.getTraceStatus';

/** The active `emma65` debug session, or `undefined` if none is active. */
function activeSession(): vscode.DebugSession | undefined {
  const session = vscode.debug.activeDebugSession;
  return session?.type === 'emma65' ? session : undefined;
}

/** Sends a trace `evaluate` context (with a JSON-encoded payload) to the active
 * session and JSON-decodes its result. Throws if there is no active session or the
 * adapter reports an error (e.g. "CPU not ready", "No trace recorded yet"). */
async function callTrace<T>(context: string, payload: unknown = {}): Promise<T> {
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

/** Opens (or reveals, if already open) the trace webview panel. */
function showTracePanel(context: vscode.ExtensionContext): void {
  if (panel) {
    panel.reveal();
    return;
  }

  panel = vscode.window.createWebviewPanel('emma65.trace', 'emma65 Trace', vscode.ViewColumn.Beside, {
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
  command: 'record' | 'stop' | 'getWindow' | 'getStatus';
  args?: { startRow: number; count: number };
}

/** Dispatches one webview request to the matching trace command and posts back a
 * `{ type: 'response', id, ok, result | error }` message. */
async function handleWebviewMessage(target: vscode.WebviewPanel, message: WebviewRequest): Promise<void> {
  if (message.type !== 'request') {
    return;
  }
  try {
    const result = await runTraceCommand(message.command, message.args);
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

async function runTraceCommand(command: WebviewRequest['command'], args?: { startRow: number; count: number }): Promise<unknown> {
  switch (command) {
    case 'record': {
      const uri = await vscode.window.showSaveDialog({ filters: { 'Trace Files': ['trace'] } });
      if (!uri) {
        return null; // user cancelled the dialog — not an error
      }
      return callTrace(RECORD_TRACE, { path: uri.fsPath });
    }
    case 'stop':
      return callTrace(STOP_TRACE);
    case 'getWindow':
      return callTrace(GET_TRACE_WINDOW, args);
    case 'getStatus':
      return callTrace(GET_TRACE_STATUS);
  }
}

/** Builds the webview's HTML shell, loading the Vite-built bundle from
 * `webview-src/dist` — or, when `EMMA65_TRACE_WEBVIEW_DEV=1` is set in the extension
 * host's environment, from a local Vite dev server for hot-reload iteration (see the
 * plan's "Development & UAT Workflow" table and the "Run Extension (trace dev
 * server)" launch config). */
function buildHtml(webview: vscode.Webview, context: vscode.ExtensionContext): string {
  const nonce = getNonce();

  if (process.env.EMMA65_TRACE_WEBVIEW_DEV === '1') {
    const devServer = 'http://localhost:5174';
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src ${devServer} 'unsafe-inline'; style-src ${devServer} 'unsafe-inline'; connect-src ${devServer} ws://localhost:5174; font-src ${devServer};" />
  <title>emma65 Trace</title>
</head>
<body>
  <div id="root"></div>
  <script type="module" src="${devServer}/@vite/client"></script>
  <script type="module" src="${devServer}/src/trace.tsx"></script>
</body>
</html>`;
  }

  const distDir = vscode.Uri.file(path.join(context.extensionPath, 'webview-src', 'dist'));
  const scriptUri = webview.asWebviewUri(vscode.Uri.joinPath(distDir, 'trace.js'));
  const styleUri = webview.asWebviewUri(vscode.Uri.joinPath(distDir, 'trace.css'));

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'nonce-${nonce}'; style-src ${webview.cspSource} 'unsafe-inline'; font-src ${webview.cspSource};" />
  <link rel="stylesheet" href="${styleUri}" />
  <title>emma65 Trace</title>
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
 * Logs `stopped` DAP events to the trace panel as a `halted` push event, driving its
 * live-follow behavior (the closest equivalent of the Tauri debugger's
 * `debugger-halted`/`debugger-run-stopped` events — see `TracePanel.tsx`'s matching
 * comment). Registered as its own `DebugAdapterTrackerFactory` alongside
 * `extension.ts`'s DAP-output-channel tracker; VS Code invokes every registered
 * factory for a given session, so this doesn't interfere with that one.
 */
class TraceLiveFollowTrackerFactory implements vscode.DebugAdapterTrackerFactory {
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

/** Registers the trace command and its DAP-event-driven live-follow tracker. */
export function registerTrace(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('emma65.showTrace', () => showTracePanel(context)),
    vscode.debug.registerDebugAdapterTrackerFactory('emma65', new TraceLiveFollowTrackerFactory()),
  );
}
