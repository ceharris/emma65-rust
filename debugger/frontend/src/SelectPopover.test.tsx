import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import SelectPopover from "./SelectPopover";

const OPTIONS = [
  { value: "a", label: "Alpha" },
  { value: "b", label: "Beta" },
  { value: "c", label: "Gamma" },
];

describe("SelectPopover", () => {
  it("renders the selected option's label on the trigger, list closed", () => {
    render(<SelectPopover label="Pick" value="b" options={OPTIONS} onChange={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Pick" })).toHaveTextContent("Beta");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("opens the list on trigger click and shows all options", async () => {
    const user = userEvent.setup();
    render(<SelectPopover label="Pick" value="a" options={OPTIONS} onChange={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "Pick" }));
    const list = screen.getByRole("listbox", { name: "Pick" });
    expect(list).toBeInTheDocument();
    expect(screen.getAllByRole("option")).toHaveLength(3);
  });

  it("marks the current value's option as selected", async () => {
    const user = userEvent.setup();
    render(<SelectPopover label="Pick" value="b" options={OPTIONS} onChange={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Pick" }));

    expect(screen.getByRole("option", { name: "Beta" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("option", { name: "Alpha" })).toHaveAttribute("aria-selected", "false");
  });

  it("calls onChange and closes when an option is clicked", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<SelectPopover label="Pick" value="a" options={OPTIONS} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Pick" }));
    await user.click(screen.getByRole("option", { name: "Gamma" }));

    expect(onChange).toHaveBeenCalledWith("c");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("closes on outside click without calling onChange", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <div>
        <SelectPopover label="Pick" value="a" options={OPTIONS} onChange={onChange} />
        <button type="button">outside</button>
      </div>,
    );

    await user.click(screen.getByRole("button", { name: "Pick" }));
    expect(screen.getByRole("listbox")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "outside" }));
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("closes on Escape", async () => {
    const user = userEvent.setup();
    render(<SelectPopover label="Pick" value="a" options={OPTIONS} onChange={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "Pick" }));
    expect(screen.getByRole("listbox")).toBeInTheDocument();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("toggles closed when the trigger is clicked again", async () => {
    const user = userEvent.setup();
    render(<SelectPopover label="Pick" value="a" options={OPTIONS} onChange={vi.fn()} />);

    const trigger = screen.getByRole("button", { name: "Pick" });
    await user.click(trigger);
    expect(screen.getByRole("listbox")).toBeInTheDocument();

    await user.click(trigger);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });
});
