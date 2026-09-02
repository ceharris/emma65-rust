import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import ColorPickerPopover from "./ColorPickerPopover";

describe("ColorPickerPopover", () => {
  it("shows the value as the swatch background and title when set", () => {
    render(<ColorPickerPopover label="Foreground" value="#123456" defaultColor="#ffffff" onChange={vi.fn()} />);
    const swatch = screen.getByRole("button", { name: "Foreground color" });
    expect(swatch).toHaveStyle({ backgroundColor: "#123456" });
    expect(swatch).toHaveAttribute("title", "Foreground: #123456");
  });

  it("falls back to defaultColor as the swatch background and title when value is null", () => {
    render(<ColorPickerPopover label="Foreground" value={null} defaultColor="#ffffff" onChange={vi.fn()} />);
    const swatch = screen.getByRole("button", { name: "Foreground color" });
    expect(swatch).toHaveStyle({ backgroundColor: "#ffffff" });
    expect(swatch).toHaveAttribute("title", "Foreground: Default (#ffffff)");
  });

  it("renders the inline label unless compact is set", () => {
    const { rerender } = render(
      <ColorPickerPopover label="Foreground" value={null} defaultColor="#ffffff" onChange={vi.fn()} />,
    );
    expect(screen.getByText("Foreground")).toBeInTheDocument();

    rerender(<ColorPickerPopover label="Foreground" value={null} defaultColor="#ffffff" onChange={vi.fn()} compact />);
    expect(screen.queryByText("Foreground")).not.toBeInTheDocument();
  });

  it("opens the popover on swatch click, showing Default and all 16 ANSI presets", async () => {
    const user = userEvent.setup();
    render(<ColorPickerPopover label="Foreground" value={null} defaultColor="#ffffff" onChange={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "Foreground color" }));
    expect(screen.getByRole("button", { name: "Default" })).toBeInTheDocument();
    expect(screen.getByTitle("Bright White")).toBeInTheDocument();
    expect(screen.getByTitle("Black")).toBeInTheDocument();
  });

  it("calls onChange(null) and closes when Default is clicked", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ColorPickerPopover label="Foreground" value="#123456" defaultColor="#ffffff" onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Foreground color" }));
    await user.click(screen.getByRole("button", { name: "Default" }));

    expect(onChange).toHaveBeenCalledWith(null);
    expect(screen.queryByRole("button", { name: "Default" })).not.toBeInTheDocument();
  });

  it("calls onChange with a preset's hex and closes when a preset swatch is clicked", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ColorPickerPopover label="Foreground" value={null} defaultColor="#ffffff" onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Foreground color" }));
    await user.click(screen.getByTitle("Bright Green"));

    expect(onChange).toHaveBeenCalledWith("#23d18b");
    expect(screen.queryByTitle("Bright Green")).not.toBeInTheDocument();
  });

  it("renders a native color input pre-filled with the current value under Custom…", async () => {
    const user = userEvent.setup();
    render(<ColorPickerPopover label="Foreground" value="#123456" defaultColor="#ffffff" onChange={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "Foreground color" }));
    const customInput = screen.getByText("Custom…").querySelector('input[type="color"]');
    expect(customInput).toHaveValue("#123456");
  });

  it("defaults the custom color input to black when value is null", async () => {
    const user = userEvent.setup();
    render(<ColorPickerPopover label="Foreground" value={null} defaultColor="#ffffff" onChange={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "Foreground color" }));
    const customInput = screen.getByText("Custom…").querySelector('input[type="color"]');
    expect(customInput).toHaveValue("#000000");
  });

  it("closes on outside click without calling onChange", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <div>
        <ColorPickerPopover label="Foreground" value={null} defaultColor="#ffffff" onChange={onChange} />
        <button type="button">outside</button>
      </div>,
    );

    await user.click(screen.getByRole("button", { name: "Foreground color" }));
    expect(screen.getByRole("button", { name: "Default" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "outside" }));
    expect(screen.queryByRole("button", { name: "Default" })).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("closes on Escape", async () => {
    const user = userEvent.setup();
    render(<ColorPickerPopover label="Foreground" value={null} defaultColor="#ffffff" onChange={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "Foreground color" }));
    expect(screen.getByRole("button", { name: "Default" })).toBeInTheDocument();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("button", { name: "Default" })).not.toBeInTheDocument();
  });
});
