import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import NumberStepper from "./NumberStepper";

/** Wraps NumberStepper with real state so typed keystrokes accumulate, mirroring how a real caller re-renders it on each onChange. */
function ControlledNumberStepper({
  initial,
  onChange,
  ...rest
}: Omit<Parameters<typeof NumberStepper>[0], "value"> & { initial: number | null }) {
  const [value, setValue] = useState<number | null>(initial);
  return (
    <NumberStepper
      {...rest}
      value={value}
      onChange={(v) => {
        setValue(v);
        onChange(v);
      }}
    />
  );
}

describe("NumberStepper", () => {
  it("renders the current value", () => {
    render(<NumberStepper value={12} onChange={vi.fn()} min={0} max={20} />);
    expect(screen.getByRole("spinbutton")).toHaveValue(12);
  });

  it("renders empty when value is null, with a placeholder", () => {
    render(<NumberStepper value={null} onChange={vi.fn()} min={0} max={20} placeholder="Default" />);
    const input = screen.getByRole("spinbutton");
    expect(input).toHaveValue(null);
    expect(input).toHaveAttribute("placeholder", "Default");
  });

  it("increments by step when the Increase button is clicked", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<NumberStepper value={10} onChange={onChange} min={0} max={20} step={2} />);

    await user.click(screen.getByRole("button", { name: "Increase" }));
    expect(onChange).toHaveBeenCalledWith(12);
  });

  it("decrements by step when the Decrease button is clicked", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<NumberStepper value={10} onChange={onChange} min={0} max={20} step={2} />);

    await user.click(screen.getByRole("button", { name: "Decrease" }));
    expect(onChange).toHaveBeenCalledWith(8);
  });

  it("treats a null value as min when adjusting", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<NumberStepper value={null} onChange={onChange} min={5} max={20} />);

    await user.click(screen.getByRole("button", { name: "Increase" }));
    expect(onChange).toHaveBeenCalledWith(6);
  });

  it("clamps adjustment at max and disables the Increase button", () => {
    render(<NumberStepper value={20} onChange={vi.fn()} min={0} max={20} />);
    expect(screen.getByRole("button", { name: "Increase" })).toBeDisabled();
  });

  it("clamps adjustment at min and disables the Decrease button", () => {
    render(<NumberStepper value={0} onChange={vi.fn()} min={0} max={20} />);
    expect(screen.getByRole("button", { name: "Decrease" })).toBeDisabled();
  });

  it("does not disable the buttons when value is null", () => {
    render(<NumberStepper value={null} onChange={vi.fn()} min={0} max={20} />);
    expect(screen.getByRole("button", { name: "Increase" })).not.toBeDisabled();
    expect(screen.getByRole("button", { name: "Decrease" })).not.toBeDisabled();
  });

  it("reports a typed value via onChange", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledNumberStepper initial={null} onChange={onChange} min={0} max={100} />);

    await user.type(screen.getByRole("spinbutton"), "42");
    expect(onChange).toHaveBeenLastCalledWith(42);
  });

  it("reports null when the input is cleared", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledNumberStepper initial={5} onChange={onChange} min={0} max={100} />);

    await user.clear(screen.getByRole("spinbutton"));
    expect(onChange).toHaveBeenLastCalledWith(null);
  });
});
