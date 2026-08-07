import * as vscode from 'vscode';
import * as path from 'path';

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

/** Extension entry point. Registers the `emma65` debug adapter type. */
export function activate(context: vscode.ExtensionContext) {
  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory('emma65', new Emma65DebugAdapterDescriptorFactory()),
  );
}

/** Extension teardown. Nothing to release yet. */
export function deactivate() {
}
