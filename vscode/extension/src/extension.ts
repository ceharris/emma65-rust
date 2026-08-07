import * as vscode from 'vscode';
import * as path from 'path';
import { registerTerminal } from './terminal';
import { registerBusCommands } from './bus';

/**
 * Resolves the `emma65-vscode-adapter` binary built by the `cargo build -p
 * emma65-vscode-adapter` pre-launch task, at `<repo-root>/target/debug/`.
 */
function adapterPath(): string {
  const name = process.platform === 'win32' ? 'emma65-vscode-adapter.exe' : 'emma65-vscode-adapter';
  return path.join(__dirname, '..', '..', '..', 'target', 'debug', name);
}

/** Spawns `emma65-vscode-adapter` as a child process communicating over stdio. */
class Emma65DebugAdapterDescriptorFactory implements vscode.DebugAdapterDescriptorFactory {
  createDebugAdapterDescriptor(
    _session: vscode.DebugSession,
    _executable: vscode.DebugAdapterExecutable | undefined,
  ): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
    return new vscode.DebugAdapterExecutable(adapterPath(), []);
  }
}

/**
 * Logs every DAP request/response/event to the "emma65 DAP" output channel, so
 * toolbar-driven UAT has something concrete to watch without needing the
 * disassembly/registers/memory panels (stories 4, 6, 7) built yet.
 */
class Emma65DebugAdapterTrackerFactory implements vscode.DebugAdapterTrackerFactory {
  constructor(private readonly output: vscode.OutputChannel) {}

  createDebugAdapterTracker(_session: vscode.DebugSession): vscode.ProviderResult<vscode.DebugAdapterTracker> {
    const output = this.output;
    return {
      onWillReceiveMessage(message: unknown) {
        output.appendLine(`-> ${JSON.stringify(message)}`);
      },
      onDidSendMessage(message: unknown) {
        output.appendLine(`<- ${JSON.stringify(message)}`);
      },
    };
  }
}

/** Extension entry point. Registers the `emma65` debug adapter type. */
export function activate(context: vscode.ExtensionContext) {
  const output = vscode.window.createOutputChannel('emma65 DAP');
  context.subscriptions.push(
    output,
    vscode.debug.registerDebugAdapterDescriptorFactory('emma65', new Emma65DebugAdapterDescriptorFactory()),
    vscode.debug.registerDebugAdapterTrackerFactory('emma65', new Emma65DebugAdapterTrackerFactory(output)),
  );
  registerTerminal(context);
  registerBusCommands(context);
}

/** Extension teardown. Nothing to release yet. */
export function deactivate() {
}
