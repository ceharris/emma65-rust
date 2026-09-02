import { act, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { EditMenuProvider, useEditMenuOverride } from "./EditMenuContext";
import { emitMockEvent, invoke, resetTauriMocks } from "./test/tauriMock";

const { readText, writeText } = vi.hoisted(() => ({
  readText: vi.fn(),
  writeText: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({ readText, writeText }));

/** Captures the live `useEditMenuOverride()` value for a test to drive from outside React. */
function Consumer({ api }: { api: { current: ReturnType<typeof useEditMenuOverride> } }) {
  api.current = useEditMenuOverride();
  return null;
}

beforeEach(() => {
  resetTauriMocks();
  readText.mockReset();
  writeText.mockReset().mockResolvedValue(undefined);
});

describe("useEditMenuOverride", () => {
  it("returns null outside an EditMenuProvider", () => {
    const { result } = renderHook(() => useEditMenuOverride());
    expect(result.current).toBeNull();
  });
});

describe("EditMenuProvider", () => {
  it("pushes all-disabled flags when nothing is focused or selected", async () => {
    render(<EditMenuProvider>{null}</EditMenuProvider>);

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("set_edit_menu_enabled", {
        flags: { cut: false, copy: false, paste: false },
      }),
    );
  });

  it("enables paste but not cut/copy for a focused input with no selection", async () => {
    render(
      <EditMenuProvider>
        <input data-testid="field" defaultValue="hello" />
      </EditMenuProvider>,
    );
    const input = screen.getByTestId("field") as HTMLInputElement;
    input.focus();
    input.setSelectionRange(0, 0);
    fireEvent.focusIn(input);

    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: false, copy: false, paste: true },
      }),
    );
  });

  it("enables cut/copy/paste for a focused input with a selection", async () => {
    render(
      <EditMenuProvider>
        <input data-testid="field" defaultValue="hello" />
      </EditMenuProvider>,
    );
    const input = screen.getByTestId("field") as HTMLInputElement;
    input.focus();
    input.setSelectionRange(0, 5);
    document.dispatchEvent(new Event("selectionchange"));

    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: true, copy: true, paste: true },
      }),
    );
  });

  it("copies the selected text on an edit-menu-action copy event", async () => {
    render(
      <EditMenuProvider>
        <input data-testid="field" defaultValue="hello" />
      </EditMenuProvider>,
    );
    const input = screen.getByTestId("field") as HTMLInputElement;
    input.focus();
    input.setSelectionRange(0, 5);
    document.dispatchEvent(new Event("selectionchange"));
    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: true, copy: true, paste: true },
      }),
    );

    emitMockEvent("edit-menu-action", "copy");

    expect(writeText).toHaveBeenCalledWith("hello");
    expect(input.value).toBe("hello");
  });

  it("cuts the selected text (copies, then removes it) on an edit-menu-action cut event", async () => {
    render(
      <EditMenuProvider>
        <input data-testid="field" defaultValue="hello" />
      </EditMenuProvider>,
    );
    const input = screen.getByTestId("field") as HTMLInputElement;
    input.focus();
    input.setSelectionRange(0, 5);
    document.dispatchEvent(new Event("selectionchange"));
    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: true, copy: true, paste: true },
      }),
    );

    emitMockEvent("edit-menu-action", "cut");

    expect(writeText).toHaveBeenCalledWith("hello");
    await waitFor(() => expect(input.value).toBe(""));
  });

  it("pastes clipboard text at the caret on an edit-menu-action paste event", async () => {
    readText.mockResolvedValueOnce("world");
    render(
      <EditMenuProvider>
        <input data-testid="field" defaultValue="hi" />
      </EditMenuProvider>,
    );
    const input = screen.getByTestId("field") as HTMLInputElement;
    input.focus();
    input.setSelectionRange(2, 2);
    fireEvent.focusIn(input);
    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: false, copy: false, paste: true },
      }),
    );

    emitMockEvent("edit-menu-action", "paste");

    await waitFor(() => expect(input.value).toBe("hiworld"));
  });

  it("registerOverride takes precedence over the default handler and reverts on unregister", async () => {
    const api: { current: ReturnType<typeof useEditMenuOverride> } = { current: null };
    render(
      <EditMenuProvider>
        <Consumer api={api} />
      </EditMenuProvider>,
    );
    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: false, copy: false, paste: false },
      }),
    );

    let unregister: (() => void) | undefined;
    act(() => {
      unregister = api.current!.registerOverride(() => ({ canCut: false, canCopy: true, canPaste: false }));
    });

    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: false, copy: true, paste: false },
      }),
    );

    act(() => unregister?.());

    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: false, copy: false, paste: false },
      }),
    );
  });

  it("notifyChanged forces an immediate recompute", async () => {
    const api: { current: ReturnType<typeof useEditMenuOverride> } = { current: null };
    let overrideCopy = false;
    render(
      <EditMenuProvider>
        <Consumer api={api} />
      </EditMenuProvider>,
    );
    act(() => {
      api.current!.registerOverride(() => ({ canCut: false, canCopy: overrideCopy, canPaste: false }));
    });
    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: false, copy: false, paste: false },
      }),
    );

    overrideCopy = true;
    act(() => api.current!.notifyChanged());

    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("set_edit_menu_enabled", {
        flags: { cut: false, copy: true, paste: false },
      }),
    );
  });
});
