import * as vscode from 'vscode';

/**
 * Custom `evaluate` contexts the adapter recognizes as NMI/IRQ/bus-signal commands —
 * see `vscode/adapter/src/bus.rs`'s doc comment for why `evaluate` is the escape hatch
 * used instead of a genuinely custom DAP request (the `dap` crate's `Command` enum is
 * closed and rejects anything it doesn't recognize).
 */
const TRIGGER_NMI = 'emma65.triggerNmi';
const ASSERT_IRQ = 'emma65.assertIrq';
const RELEASE_IRQ = 'emma65.releaseIrq';
const GET_BUS_STATE = 'emma65.getBusState';

/** The active `emma65` debug session, or `undefined` if none is active. */
function activeSession(): vscode.DebugSession | undefined {
  const session = vscode.debug.activeDebugSession;
  return session?.type === 'emma65' ? session : undefined;
}

/**
 * Sends one of the custom bus/interrupt contexts to the active session via `evaluate`
 * and shows its one-line result (`vscode/adapter/src/bus.rs`'s `CpuBusState::describe`)
 * as an information message.
 */
async function sendBusCommand(context: string): Promise<void> {
  const session = activeSession();
  if (!session) {
    vscode.window.showWarningMessage('emma65: no active debug session.');
    return;
  }
  try {
    const response = await session.customRequest('evaluate', { expression: '', context });
    vscode.window.showInformationMessage(`emma65: ${response.result}`);
  } catch (err) {
    vscode.window.showErrorMessage(`emma65: ${err instanceof Error ? err.message : String(err)}`);
  }
}

/** Registers the NMI/IRQ/bus-signal commands, invoked from the command palette or the
 * debug toolbar (`contributes.commands`/`contributes.menus` in `package.json`). */
export function registerBusCommands(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('emma65.triggerNmi', () => sendBusCommand(TRIGGER_NMI)),
    vscode.commands.registerCommand('emma65.assertIrq', () => sendBusCommand(ASSERT_IRQ)),
    vscode.commands.registerCommand('emma65.releaseIrq', () => sendBusCommand(RELEASE_IRQ)),
    vscode.commands.registerCommand('emma65.showBusState', () => sendBusCommand(GET_BUS_STATE)),
  );
}
