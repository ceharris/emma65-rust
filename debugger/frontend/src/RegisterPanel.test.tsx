import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ReactNode, useEffect } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ExecutionProvider, useExecutionContext } from "./ExecutionContext";
import RegisterPanel, { RegisterSnapshot } from "./RegisterPanel";
import { invoke, resetTauriMocks } from "./test/tauriMock";

function snapshot(overrides: Partial<RegisterSnapshot> = {}): RegisterSnapshot {
  return {
    a: 0x42, x: 0x01, y: 0xff, s: 0xfd, pc: 0x8000, p: 0x20, changed_flags: 0,
    cpu_stopped: false, cpu_waiting: false, breakpoint_hit: false,
    ...overrides,
  };
}

/** Drives `execState` via the real `ExecutionContext` before rendering children, so tests can exercise `isEditable`. */
function ExecStateSetter({ execState }: { execState: "stopped" | "stepping" | "running" }) {
  const { onExecStateChange } = useExecutionContext();
  useEffect(() => {
    onExecStateChange(execState);
  }, [execState, onExecStateChange]);
  return null;
}

function Providers({ children, execState = "stopped" }: { children: ReactNode; execState?: "stopped" | "stepping" | "running" }) {
  return (
    <ExecutionProvider>
      <ExecStateSetter execState={execState} />
      {children}
    </ExecutionProvider>
  );
}

function renderPanel(execState: "stopped" | "stepping" | "running" = "stopped") {
  return render(<RegisterPanel />, { wrapper: (props) => <Providers execState={execState}>{props.children}</Providers> });
}

beforeEach(() => {
  resetTauriMocks();
});

describe("RegisterPanel", () => {
  it("shows a waiting placeholder, then the fetched registers formatted in hex", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot() : undefined));
    renderPanel();

    expect(screen.getByText("Waiting…")).toBeInTheDocument();

    const aRow = (await screen.findByText("A")).closest("tr")!;
    expect(within(aRow).getByText("42")).toBeInTheDocument();
    expect(within(aRow).getByText("'B'")).toBeInTheDocument();

    expect(within(screen.getByText("X").closest("tr")!).getByText("01")).toBeInTheDocument();
    expect(within(screen.getByText("Y").closest("tr")!).getByText("FF")).toBeInTheDocument();
    expect(within(screen.getByText("PC").closest("tr")!).getByText("8000")).toBeInTheDocument();
    expect(within(screen.getByText("S").closest("tr")!).getByText("FD")).toBeInTheDocument();
  });

  it("does not show an ASCII hint for a non-printable A value", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot({ a: 0x00 }) : undefined));
    renderPanel();

    const aRow = (await screen.findByText("A")).closest("tr")!;
    expect(within(aRow).getByText("00")).toBeInTheDocument();
    expect(within(aRow).queryByText(/^'.*'$/)).not.toBeInTheDocument();
  });

  it("cycling the data radix reformats A/X/Y but not PC/S (a separate radix cycle)", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot({ a: 0x0a }) : undefined));
    renderPanel();
    const aRow = (await screen.findByText("A")).closest("tr")!;
    expect(within(aRow).getByText("0A")).toBeInTheDocument();

    const [dataRadixButton] = screen.getAllByTitle("Cycle radix");
    await user.click(dataRadixButton); // hex -> udec

    expect(within(aRow).getByText("10")).toBeInTheDocument();
    expect(within(screen.getByText("PC").closest("tr")!).getByText("8000")).toBeInTheDocument();
  });

  it("double-clicking a register value opens an editable input; Enter commits set_register", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => {
      if (cmd === "get_registers") return snapshot();
      if (cmd === "set_register") return snapshot({ a: 0xff });
      return undefined;
    });
    renderPanel();
    const aRow = (await screen.findByText("A")).closest("tr")!;

    await user.dblClick(within(aRow).getByText("42"));
    const input = within(aRow).getByRole("textbox");
    await user.clear(input);
    await user.type(input, "FF{Enter}");

    expect(invoke).toHaveBeenCalledWith("set_register", { field: "a", value: 0xff });
  });

  it("double-clicking a register value auto-selects the input text", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot() : undefined));
    renderPanel();
    const aRow = (await screen.findByText("A")).closest("tr")!;

    await user.dblClick(within(aRow).getByText("42"));
    const input = within(aRow).getByRole("textbox") as HTMLInputElement;

    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe(input.value.length);
  });

  it("Escape cancels an in-progress edit without committing", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot() : undefined));
    renderPanel();
    const aRow = (await screen.findByText("A")).closest("tr")!;

    await user.dblClick(within(aRow).getByText("42"));
    await user.type(within(aRow).getByRole("textbox"), "FF");
    await user.keyboard("{Escape}");

    expect(within(aRow).getByText("42")).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("set_register", expect.anything());
  });

  it("an out-of-range value marks the input invalid and does not commit", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot() : undefined));
    renderPanel();
    const aRow = (await screen.findByText("A")).closest("tr")!;

    await user.dblClick(within(aRow).getByText("42"));
    const input = within(aRow).getByRole("textbox");
    await user.clear(input);
    await user.type(input, "256{Enter}"); // out of range for an 8-bit register

    expect(input).toHaveClass("invalid");
    expect(invoke).not.toHaveBeenCalledWith("set_register", expect.anything());
  });

  it("PC rejects a signed value since it has no signed display mode", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot() : undefined));
    renderPanel();
    const pcRow = (await screen.findByText("PC")).closest("tr")!;

    await user.dblClick(within(pcRow).getByText("8000"));
    const input = within(pcRow).getByRole("textbox");
    await user.clear(input);
    await user.type(input, "-1{Enter}");

    expect(input).toHaveClass("invalid");
    expect(invoke).not.toHaveBeenCalledWith("set_register", expect.anything());
  });

  it("editing is disabled while the CPU is not stopped", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot() : undefined));
    const user = userEvent.setup();
    renderPanel("running");
    const aRow = (await screen.findByText("A")).closest("tr")!;

    await user.dblClick(within(aRow).getByText("42"));

    expect(within(aRow).queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("double-clicking the flags cell opens a toggle editor; clicking a flag then Enter commits", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => {
      if (cmd === "get_registers") return snapshot({ p: 0x20 });
      if (cmd === "set_register") return snapshot({ p: 0x21 });
      return undefined;
    });
    renderPanel();
    await screen.findByText("A");

    const flagsCell = document.querySelector(".reg-flags")!;
    await user.dblClick(within(flagsCell as HTMLElement).getAllByText("-")[0]);

    // With p=0x20 (only the always-"-" unused bit set), all 8 flag positions
    // display "-" in edit mode; FLAG_CHARS orders them N V - B D I Z C, so the
    // last of the 8 is C.
    const dashSpans = within(flagsCell as HTMLElement).getAllByText("-");
    const carryFlag = dashSpans[dashSpans.length - 1];
    await user.click(carryFlag);
    await user.keyboard("{Enter}");

    expect(invoke).toHaveBeenCalledWith("set_register", { field: "p", value: 0x21 });
  });

  it("Escape cancels a flags edit without committing", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_registers" ? snapshot({ p: 0x20 }) : undefined));
    renderPanel();
    await screen.findByText("A");

    const flagsCell = document.querySelector(".reg-flags")!;
    await user.dblClick(within(flagsCell as HTMLElement).getAllByText("-")[0]);
    await user.keyboard("{Escape}");

    expect(invoke).not.toHaveBeenCalledWith("set_register", { field: "p", value: expect.anything() });
  });
});
