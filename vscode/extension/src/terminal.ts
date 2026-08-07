import * as vscode from 'vscode';
import * as net from 'net';
import * as os from 'os';
import * as path from 'path';

/**
 * Unix-domain socket the emulator's console device listens on once a debug session has
 * launched — see `vscode/adapter/src/session.rs`'s `TERMINAL_SOCKET_SPEC`. Connecting to
 * it directly, independent of the DAP connection, sidesteps the `dap` crate's total lack
 * of support for custom request/event names (see that module's doc comment for the full
 * rationale).
 */
function terminalSocketPath(): string {
  return path.join(os.homedir(), '.emma', 'sock', 'vscode-terminal');
}

const MAX_CONNECT_ATTEMPTS = 10;
const RECONNECT_DELAY_MS = 200;

/**
 * Bridges the emulator's console byte stream to a VS Code integrated terminal tab.
 *
 * Connects directly to the Unix socket the console device listens on, as a raw
 * byte-stream peer — the same role `util/via_sr_peripheral.py` plays for the VIA's
 * transport. Bytes are carried as JS strings one code point per byte (`latin1`), not
 * decoded as UTF-8: the console's output is an arbitrary byte stream (control codes
 * included), and `vscode.Pseudoterminal` only accepts `string`, not raw bytes.
 */
class Emma65Pseudoterminal implements vscode.Pseudoterminal {
  private readonly writeEmitter = new vscode.EventEmitter<string>();
  private readonly closeEmitter = new vscode.EventEmitter<number | void>();
  onDidWrite = this.writeEmitter.event;
  onDidClose = this.closeEmitter.event;

  private socket: net.Socket | undefined;
  private closed = false;
  private connected = false;

  open(): void {
    this.connect(0);
  }

  close(): void {
    this.closed = true;
    this.socket?.destroy();
    this.socket = undefined;
  }

  handleInput(data: string): void {
    this.socket?.write(Buffer.from(data, 'latin1'));
  }

  private connect(attempt: number): void {
    if (this.closed) {
      return;
    }
    const socket = net.createConnection({ path: terminalSocketPath() });
    this.socket = socket;

    socket.on('connect', () => {
      this.connected = true;
    });
    socket.on('data', (chunk: Buffer) => {
      this.writeEmitter.fire(chunk.toString('latin1'));
    });
    socket.on('error', (err) => {
      if (this.connected) {
        this.writeEmitter.fire(`\r\nemma65: console connection error: ${err.message}\r\n`);
      }
    });
    // A failed connection attempt (the adapter's `launch` may not have finished
    // creating the socket yet) surfaces as 'close', same as a real disconnect after a
    // successful 'connect' — `connected` tells the two apart, since Node emits 'close'
    // after 'error' in both cases.
    socket.on('close', () => {
      if (this.closed) {
        return;
      }
      if (this.connected) {
        this.closeEmitter.fire();
        return;
      }
      if (attempt + 1 >= MAX_CONNECT_ATTEMPTS) {
        this.writeEmitter.fire('\r\nemma65: failed to connect to the emulator console.\r\n');
        this.closeEmitter.fire();
        return;
      }
      setTimeout(() => this.connect(attempt + 1), RECONNECT_DELAY_MS);
    });
  }
}

let activeTerminal: vscode.Terminal | undefined;

/** Opens (or focuses, if already open) the emma65 integrated console terminal. */
export function openEmma65Terminal(): void {
  if (activeTerminal) {
    activeTerminal.show();
    return;
  }
  const pty = new Emma65Pseudoterminal();
  activeTerminal = vscode.window.createTerminal({ name: 'emma65 Console', pty });
  activeTerminal.show(true);
}

/** Registers the terminal command and the debug-session lifecycle hooks that open and
 * tear down the console terminal alongside an `emma65` debug session. */
export function registerTerminal(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('emma65.openTerminal', openEmma65Terminal),
    vscode.window.onDidCloseTerminal((terminal) => {
      if (terminal === activeTerminal) {
        activeTerminal = undefined;
      }
    }),
    vscode.debug.onDidStartDebugSession((session) => {
      if (session.type === 'emma65') {
        openEmma65Terminal();
      }
    }),
    vscode.debug.onDidTerminateDebugSession((session) => {
      if (session.type === 'emma65' && activeTerminal) {
        activeTerminal.dispose();
      }
    }),
  );
}
