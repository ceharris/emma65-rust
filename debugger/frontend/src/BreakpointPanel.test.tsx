import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import BreakpointPanel from "./BreakpointPanel";
import { ExecutionProvider } from "./ExecutionContext";
import { PanelHeaderActionProvider, usePanelHeaderActions } from "./layout/panelHeaderActions";
import { RunControlsProvider } from "./RunControlsContext";
import { emitMockEvent, invoke, resetTauriMocks } from "./test/tauriMock";

interface BreakpointInfo {
  addr: number;
  enabled: boolean;
  label: string | null;
}

function breakpoint(overrides: Partial<BreakpointInfo> = {}): BreakpointInfo {
  return { addr: 0x1234, enabled: true, label: null, ...overrides };
}

/** Renders the "breakpoints" panel-header action (the Add breakpoint button) as `DockTabActions` would. */
function HeaderActionButton() {
  const actions = usePanelHeaderActions();
  const action = actions.breakpoints;
  if (!action) return null;
  return (
    <button onClick={action.onClick} disabled={action.disabled} title={action.disabledTitle ?? action.title}>
      {action.title}
    </button>
  );
}

function Providers({ children }: { children: ReactNode }) {
  return (
    <ExecutionProvider>
      <RunControlsProvider>
        <PanelHeaderActionProvider>
          <HeaderActionButton />
          {children}
        </PanelHeaderActionProvider>
      </RunControlsProvider>
    </ExecutionProvider>
  );
}

beforeEach(() => {
  resetTauriMocks();
});

describe("BreakpointPanel", () => {
  it("shows a waiting placeholder, then an empty state when there are no breakpoints", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_breakpoints" ? [] : undefined));
    render(<BreakpointPanel />, { wrapper: Providers });

    expect(screen.getByText("Waiting…")).toBeInTheDocument();
    expect(await screen.findByText("No breakpoints")).toBeInTheDocument();
  });

  it("renders fetched breakpoints, formatting the address and showing the label when present", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) =>
      cmd === "get_breakpoints" ? [breakpoint({ addr: 0x55aa, label: "reset_vec" }), breakpoint({ addr: 0x02, enabled: false })] : undefined,
    );
    render(<BreakpointPanel />, { wrapper: Providers });

    expect(await screen.findByText("55AA")).toBeInTheDocument();
    expect(screen.getByText("reset_vec")).toBeInTheDocument();
    expect(screen.getByText("0002")).toBeInTheDocument();
  });

  it("updates rows when a breakpoints-changed event arrives", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_breakpoints" ? [] : undefined));
    render(<BreakpointPanel />, { wrapper: Providers });
    await screen.findByText("No breakpoints");

    act(() => emitMockEvent("breakpoints-changed", [breakpoint({ addr: 0x0300 })]));

    expect(await screen.findByText("0300")).toBeInTheDocument();
  });

  it("toggling the indicator disables an enabled breakpoint and enables a disabled one", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) =>
      cmd === "get_breakpoints"
        ? [breakpoint({ addr: 0x1000, enabled: true }), breakpoint({ addr: 0x2000, enabled: false })]
        : undefined,
    );
    render(<BreakpointPanel />, { wrapper: Providers });
    await screen.findByText("1000");

    await user.click(screen.getByTitle("Disable breakpoint"));
    expect(invoke).toHaveBeenCalledWith("disable_breakpoint", { addr: 0x1000 });

    await user.click(screen.getByTitle("Enable breakpoint"));
    expect(invoke).toHaveBeenCalledWith("enable_breakpoint", { addr: 0x2000 });
  });

  it("removing a breakpoint invokes remove_breakpoint with its address", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_breakpoints" ? [breakpoint({ addr: 0x4000 })] : undefined));
    render(<BreakpointPanel />, { wrapper: Providers });
    await screen.findByText("4000");

    await user.click(screen.getByTitle("Remove breakpoint"));

    expect(invoke).toHaveBeenCalledWith("remove_breakpoint", { addr: 0x4000 });
  });

  it("disables editing controls while the CPU is running", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => {
      if (cmd === "get_breakpoints") return [breakpoint({ addr: 0x1000, enabled: true })];
      return undefined;
    });
    const { container } = render(<BreakpointPanel />, { wrapper: Providers });
    await screen.findByText("1000");

    act(() => emitMockEvent("run-menu-action", "run-cpu"));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("run_cpu"));
    vi.mocked(invoke).mockClear();

    const removeButton = container.querySelector(".breakpoint-remove-btn");
    expect(removeButton).toBeDisabled();
    expect(screen.getByRole("button", { name: "Add breakpoint" })).toBeDisabled();

    const indicator = container.querySelector(".indicator")!;
    expect(indicator).toHaveClass("readonly");
    await user.click(indicator);
    expect(invoke).not.toHaveBeenCalledWith(expect.stringMatching(/breakpoint/), expect.anything());
  });

  it("add-breakpoint dialog: empty input is rejected, then a hex address is accepted", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => {
      if (cmd === "get_breakpoints") return [];
      if (cmd === "resolve_symbol") return null;
      return undefined;
    });
    render(<BreakpointPanel />, { wrapper: Providers });
    await screen.findByText("No breakpoints");

    await user.click(screen.getByRole("button", { name: "Add breakpoint" }));
    expect(screen.getByText("Add Breakpoint")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "OK" }));
    expect(screen.getByText("Enter an address or symbol")).toBeInTheDocument();

    await user.type(screen.getByPlaceholderText("e.g. 55AA or a symbol"), "55AA");
    await user.click(screen.getByRole("button", { name: "OK" }));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("set_breakpoint", { addr: 0x55aa }));
    expect(screen.queryByText("Add Breakpoint")).not.toBeInTheDocument();
  });

  it("add-breakpoint dialog: an unresolved, non-hex symbol shows an error and does not set a breakpoint", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => {
      if (cmd === "get_breakpoints") return [];
      if (cmd === "resolve_symbol") return null;
      return undefined;
    });
    render(<BreakpointPanel />, { wrapper: Providers });
    await screen.findByText("No breakpoints");

    await user.click(screen.getByRole("button", { name: "Add breakpoint" }));
    await user.type(screen.getByPlaceholderText("e.g. 55AA or a symbol"), "not_a_symbol!!");
    await user.click(screen.getByRole("button", { name: "OK" }));

    expect(await screen.findByText("Unrecognized address or symbol")).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("set_breakpoint", expect.anything());
  });

  it("Escape closes the add-breakpoint dialog", async () => {
    const user = userEvent.setup();
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === "get_breakpoints" ? [] : undefined));
    render(<BreakpointPanel />, { wrapper: Providers });
    await screen.findByText("No breakpoints");

    await user.click(screen.getByRole("button", { name: "Add breakpoint" }));
    expect(screen.getByText("Add Breakpoint")).toBeInTheDocument();

    await user.keyboard("{Escape}");

    expect(screen.queryByText("Add Breakpoint")).not.toBeInTheDocument();
  });
});
