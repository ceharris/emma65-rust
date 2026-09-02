import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { RadixButton, type DataRadix } from "./RadixControl";

describe("RadixButton", () => {
  it("renders the current radix's label", () => {
    render(<RadixButton radix="hex" onCycle={vi.fn()} />);
    expect(screen.getByRole("button")).toHaveTextContent("HEX");
  });

  it("renders each radix's expected label", () => {
    const labels: Record<DataRadix, string> = { hex: "HEX", udec: "DEC", sdec: "±DEC", oct: "OCT", bin: "BIN" };
    for (const [radix, label] of Object.entries(labels) as [DataRadix, string][]) {
      const { unmount } = render(<RadixButton radix={radix} onCycle={vi.fn()} />);
      expect(screen.getByRole("button")).toHaveTextContent(label);
      unmount();
    }
  });

  it("calls onCycle when clicked", async () => {
    const user = userEvent.setup();
    const onCycle = vi.fn();
    render(<RadixButton radix="hex" onCycle={onCycle} />);

    await user.click(screen.getByRole("button"));
    expect(onCycle).toHaveBeenCalledTimes(1);
  });

  it("does not stop propagation by default", async () => {
    const user = userEvent.setup();
    const outerClick = vi.fn();
    render(
      <div onClick={outerClick}>
        <RadixButton radix="hex" onCycle={vi.fn()} />
      </div>,
    );

    await user.click(screen.getByRole("button"));
    expect(outerClick).toHaveBeenCalledTimes(1);
  });

  it("stops propagation to a parent click handler when stopPropagation is set", async () => {
    const user = userEvent.setup();
    const outerClick = vi.fn();
    const onCycle = vi.fn();
    render(
      <div onClick={outerClick}>
        <RadixButton radix="hex" onCycle={onCycle} stopPropagation />
      </div>,
    );

    await user.click(screen.getByRole("button"));
    expect(onCycle).toHaveBeenCalledTimes(1);
    expect(outerClick).not.toHaveBeenCalled();
  });
});
