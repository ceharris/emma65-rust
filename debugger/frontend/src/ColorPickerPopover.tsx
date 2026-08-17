import { useEffect, useRef, useState } from "react";
import { ANSI_PRESET_COLORS } from "./terminalPreferences";
import "./styles/modal.scss";

interface ColorPickerPopoverProps {
  label: string;
  /** `null` means "use the current light/dark theme's value" (the "Default" option below). */
  value: string | null;
  /**
   * The color actually in effect when `value` is `null` — shown as the
   * swatch's own background instead of a placeholder, so an unset field
   * previews what the terminal is really rendering rather than a generic
   * "unset" marker.
   */
  defaultColor: string;
  onChange: (value: string | null) => void;
  /**
   * Omits the inline text label next to the swatch (the name is still
   * available via `title`/`aria-label`) — used by the 16-entry palette grid
   * so 8 columns fit without forcing the dialog wide; the foreground/
   * background row keeps its label visible since it only has two entries.
   */
  compact?: boolean;
}

/**
 * A labeled swatch button that opens a small popover offering: a "Default"
 * option (clears back to `null`, following the current light/dark theme), 16
 * one-click ANSI reference-color presets, and a "Custom…" swatch backed by
 * the OS-native `<input type="color">` for arbitrary 24-bit RGB — per the
 * terminal-preferences plan doc's "Color chooser" decision. Built for the
 * Preferences dialog's Text tab (foreground/background/16-palette), reused
 * as-is by Work Unit 3 for the Cursor tab's cursor/accent colors.
 */
export default function ColorPickerPopover({ label, value, defaultColor, onChange, compact = false }: ColorPickerPopoverProps) {
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

  const choose = (hex: string | null) => {
    onChange(hex);
    setOpen(false);
  };

  return (
    <div className="color-picker" ref={containerRef}>
      <button
        type="button"
        className="color-picker-swatch"
        style={{ backgroundColor: value ?? defaultColor }}
        title={value ? `${label}: ${value}` : `${label}: Default (${defaultColor})`}
        aria-label={`${label} color`}
        onClick={() => setOpen((o) => !o)}
      />
      {!compact && <span className="color-picker-label">{label}</span>}

      {open && (
        <div className="color-picker-popover" onKeyDown={(e) => e.stopPropagation()}>
          <button type="button" className="color-picker-default-option" onClick={() => choose(null)}>
            Default
          </button>
          <div className="color-picker-presets">
            {ANSI_PRESET_COLORS.map((c) => (
              <button
                key={c.hex}
                type="button"
                className="color-picker-preset"
                style={{ backgroundColor: c.hex }}
                title={c.label}
                onClick={() => choose(c.hex)}
              />
            ))}
          </div>
          <label className="color-picker-custom">
            Custom…
            <input type="color" value={value ?? "#000000"} onChange={(e) => choose(e.target.value)} />
          </label>
        </div>
      )}
    </div>
  );
}
