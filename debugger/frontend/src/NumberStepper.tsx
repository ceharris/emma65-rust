import "./styles/modal.scss";

interface NumberStepperProps {
  /** `null` means "unset" (e.g. font size's "use platform default"). */
  value: number | null;
  onChange: (value: number | null) => void;
  min: number;
  max: number;
  step?: number;
  placeholder?: string;
}

/**
 * A themed replacement for `<input type="number">`'s increment/decrement
 * spinner — WebKitGTK (this app's Linux webview) renders that spinner as
 * unthemeable OS-native chrome, the same class of defect
 * `SelectPopover.tsx`/`ColorPickerPopover.tsx` were built to fix elsewhere
 * in the Preferences dialog (issue #467). Keeps the underlying
 * `<input type="number">` itself — so typed numeric entry, keyboard
 * up/down, and native `min`/`max` behavior all keep working — but hides its
 * native spinner via `modal.scss`'s `.number-stepper .modal-input` rule and
 * adds a hand-drawn +/- button stack next to it instead. Used by the
 * Preferences dialog's Text tab (font size, scrollback).
 */
export default function NumberStepper({ value, onChange, min, max, step = 1, placeholder }: NumberStepperProps) {
  const clamp = (n: number) => Math.min(max, Math.max(min, n));
  const adjust = (delta: number) => onChange(clamp((value ?? min) + delta));

  return (
    <div className="number-stepper">
      <input
        type="number"
        className="modal-input modal-input-narrow"
        min={min}
        max={max}
        step={step}
        placeholder={placeholder}
        value={value ?? ""}
        onChange={(e) => onChange(e.target.value ? Number(e.target.value) : null)}
      />
      <div className="number-stepper-buttons">
        <button
          type="button"
          className="number-stepper-btn"
          aria-label="Increase"
          disabled={value != null && value >= max}
          onClick={() => adjust(step)}
        >
          ▲
        </button>
        <button
          type="button"
          className="number-stepper-btn"
          aria-label="Decrease"
          disabled={value != null && value <= min}
          onClick={() => adjust(-step)}
        >
          ▼
        </button>
      </div>
    </div>
  );
}
