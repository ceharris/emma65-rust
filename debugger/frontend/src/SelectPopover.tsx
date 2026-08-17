import { useEffect, useRef, useState } from "react";
import "./styles/modal.scss";

interface SelectPopoverOption<T extends string> {
  value: T;
  label: string;
}

interface SelectPopoverProps<T extends string> {
  /** Accessible name for the option list (`aria-label`) — not rendered inline. */
  label: string;
  value: T;
  options: SelectPopoverOption<T>[];
  onChange: (value: T) => void;
}

/**
 * A labeled trigger button that opens a small popover list of text options —
 * the `<select>`/`<option>` equivalent of `ColorPickerPopover`, built for the
 * same reason: a plain `<select>`'s closed box can be restyled with CSS, but
 * its open dropdown list is OS-native chrome that WebKitGTK (this app's
 * Linux webview) renders outside the page's theme entirely, the same defect
 * already fixed for radio buttons via `modal.scss`'s `.modal-template-row`
 * (`appearance: none` + a hand-drawn indicator) — except a `<select>`'s
 * *list portion* isn't reachable through `appearance` at all, so nothing
 * native can be used here, not even for the closed box. Used by the
 * Preferences dialog's Cursor tab (issue #467 Work Unit 3) for the
 * active/inactive shape pickers; Work Unit 4's Backspace/Delete pickers
 * reuse it as-is.
 */
export default function SelectPopover<T extends string>({ label, value, options, onChange }: SelectPopoverProps<T>) {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handleClickOutside = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) setOpen(false);
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.stopPropagation(); setOpen(false); }
    };
    document.addEventListener("mousedown", handleClickOutside);
    document.addEventListener("keydown", handleKeyDown, true);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleKeyDown, true);
    };
  }, [open]);

  const selectedLabel = options.find((o) => o.value === value)?.label ?? value;

  return (
    <div className="select-popover" ref={containerRef}>
      <button
        type="button"
        className="select-popover-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={label}
        onClick={() => setOpen((o) => !o)}
      >
        <span>{selectedLabel}</span>
        <span className="select-popover-caret" aria-hidden="true" />
      </button>

      {open && (
        <div className="select-popover-list" role="listbox" aria-label={label} onKeyDown={(e) => e.stopPropagation()}>
          {options.map((o) => (
            <button
              key={o.value}
              type="button"
              role="option"
              aria-selected={o.value === value}
              className={`select-popover-option${o.value === value ? " active" : ""}`}
              onClick={() => { onChange(o.value); setOpen(false); }}
            >
              {o.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
