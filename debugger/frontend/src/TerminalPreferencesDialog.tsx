import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./styles/modal.scss";
import { isMonospaceFont } from "./terminalSizing";
import ColorPickerPopover from "./ColorPickerPopover";
import NumberStepper from "./NumberStepper";
import SelectPopover from "./SelectPopover";
import {
  ANSI_PALETTE_FIELDS,
  CursorInactiveShape,
  CursorShape,
  TerminalCompatibilityPreferences,
  TerminalCursorPreferences,
  TerminalKeyAction,
  TerminalPreferences,
  TerminalTextPreferences,
} from "./terminalPreferences";

const ACTIVE_SHAPE_OPTIONS: { value: CursorShape; label: string }[] = [
  { value: "block", label: "Block" },
  { value: "underline", label: "Underline" },
  { value: "bar", label: "Bar" },
];

const INACTIVE_SHAPE_OPTIONS: { value: CursorInactiveShape; label: string }[] = [
  { value: "outline", label: "Outline" },
  { value: "block", label: "Block" },
  { value: "underline", label: "Underline" },
  { value: "bar", label: "Bar" },
  { value: "none", label: "None" },
];

const KEY_ACTION_OPTIONS: { value: TerminalKeyAction; label: string }[] = [
  { value: "bs", label: "ASCII Backspace (BS)" },
  { value: "del", label: "ASCII Delete (DEL)" },
  { value: "dch", label: "ANSI Delete Character (DCH)" },
];

interface TerminalPreferencesDialogProps {
  open: boolean;
  onClose: () => void;
  /**
   * Called with the saved preferences right after a successful
   * `set_terminal_preferences`, so the caller (`TerminalPanel`) can apply
   * the Text-tab-relevant values to the already-open live terminal.
   */
  onSaved: (preferences: TerminalPreferences) => void;
  /**
   * The current light/dark base theme's foreground/background — previewed
   * by the foreground/background color pickers when those fields are unset,
   * so an unset swatch shows what the terminal is actually rendering rather
   * than a generic placeholder.
   */
  defaultForeground: string;
  defaultBackground: string;
  /**
   * Same idea as `defaultForeground`/`defaultBackground`, for the Cursor
   * tab's color/accent-color swatches: the current light/dark base theme's
   * `cursor` value, and `@xterm/xterm`'s own built-in `cursorAccent` default
   * (`DEFAULT_CURSOR_ACCENT_COLOR` — independent of theme, see its doc
   * comment in `terminalPreferences.ts`).
   */
  defaultCursorColor: string;
  defaultCursorAccentColor: string;
}

type TabId = "text" | "cursor" | "compatibility";
const TABS: { id: TabId; label: string }[] = [
  { id: "text", label: "Text" },
  { id: "cursor", label: "Cursor" },
  { id: "compatibility", label: "Compatibility" },
];

/**
 * The terminal's "Preferences…" modal (issue #467), opened from
 * `TerminalPanel.tsx`'s size-preset menu. Follows the existing dialog
 * pattern (`AboutDialog.tsx`/`NewProfileDialog.tsx`: local state, manual
 * Escape/Enter handling, `modal.scss` classes) since there's no shared
 * `<Modal>` component. Mounted directly by `TerminalPanel` (not globally in
 * `App.tsx` like `AboutDialog`) since it needs `open`/`onSaved` wired to
 * whichever terminal instance — docked or detached — actually opened it.
 */
export default function TerminalPreferencesDialog({
  open,
  onClose,
  onSaved,
  defaultForeground,
  defaultBackground,
  defaultCursorColor,
  defaultCursorAccentColor,
}: TerminalPreferencesDialogProps) {
  const [preferences, setPreferences] = useState<TerminalPreferences | null>(null);
  const [fontError, setFontError] = useState("");
  const [tab, setTab] = useState<TabId>("text");
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (!open) return;
    setTab("text");
    setFontError("");
    invoke<TerminalPreferences>("get_terminal_preferences")
      .then(setPreferences)
      .catch((err) => console.error("get_terminal_preferences failed:", err));
  }, [open]);

  const updateText = (patch: Partial<TerminalTextPreferences>) => {
    setPreferences((p) => p && { ...p, text: { ...p.text, ...patch } });
    if ("font_family" in patch) setFontError("");
  };

  const updateCursor = (patch: Partial<TerminalCursorPreferences>) => {
    setPreferences((p) => p && { ...p, cursor: { ...p.cursor, ...patch } });
  };

  const updateCompatibility = (patch: Partial<TerminalCompatibilityPreferences>) => {
    setPreferences((p) => p && { ...p, compatibility: { ...p.compatibility, ...patch } });
  };

  /**
   * Validates the font family (per the plan's "Font face" decision: an
   * unknown/non-monospace name blocks Save with an inline error rather than
   * silently falling back), then persists the whole preferences struct and
   * applies it live via `onSaved`.
   */
  const commit = async () => {
    if (!preferences || submitting) return;
    const fontFamily = preferences.text.font_family?.trim() || null;
    if (fontFamily && !isMonospaceFont(fontFamily)) {
      setFontError(`"${fontFamily}" isn't installed, or isn't a monospace font`);
      return;
    }
    const toSave: TerminalPreferences = { ...preferences, text: { ...preferences.text, font_family: fontFamily } };
    setSubmitting(true);
    try {
      await invoke("set_terminal_preferences", { preferences: toSave });
      onSaved(toSave);
      onClose();
    } catch (err) {
      setFontError(String(err));
    } finally {
      setSubmitting(false);
    }
  };

  if (!open || !preferences) return null;

  const { text, cursor, compatibility } = preferences;

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-dialog modal-dialog-wide"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          // Stops app-wide shortcuts (e.g. Ctrl+N) from firing while typing
          // here, same concern `NewProfileDialog` handles per-input — a
          // single wrapper on the dialog covers every field at once.
          e.stopPropagation();
          if (e.key === "Escape") { e.preventDefault(); onClose(); }
          if (e.key === "Enter") { e.preventDefault(); commit(); }
        }}
      >
        <div className="modal-title">Terminal Preferences</div>

        <div className="modal-tabs" role="tablist">
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={tab === t.id}
              className={`modal-tab${tab === t.id ? " active" : ""}`}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>

        {tab === "text" && (
          <div className="modal-tab-panel">
            <div className="modal-field">
              <label className="modal-label">Font family</label>
              <input
                className={`modal-input${fontError ? " invalid" : ""}`}
                spellCheck={false}
                placeholder="platform default"
                value={text.font_family ?? ""}
                onChange={(e) => updateText({ font_family: e.target.value || null })}
              />
            </div>
            {fontError && <div className="modal-error">{fontError}</div>}

            <div className="modal-field">
              <label className="modal-label">Font size</label>
              <NumberStepper
                min={6}
                max={72}
                placeholder="default"
                value={text.font_size}
                onChange={(v) => updateText({ font_size: v })}
              />
            </div>

            <div className="modal-field">
              <label className="modal-label">Scrollback</label>
              <NumberStepper
                min={0}
                max={100000}
                value={text.scrollback}
                onChange={(v) => updateText({ scrollback: v ?? 0 })}
              />
            </div>

            <div className="modal-field">
              <label className="modal-label">Colors</label>
              <div className="color-picker-row">
                <ColorPickerPopover
                  label="Foreground"
                  value={text.foreground}
                  defaultColor={defaultForeground}
                  onChange={(v) => updateText({ foreground: v })}
                />
                <ColorPickerPopover
                  label="Background"
                  value={text.background}
                  defaultColor={defaultBackground}
                  onChange={(v) => updateText({ background: v })}
                />
              </div>
            </div>

            <div className="modal-field modal-palette-field">
              <label className="modal-label">Palette</label>
              <div className="color-picker-grid">
                {ANSI_PALETTE_FIELDS.map(({ field, label, defaultHex }) => (
                  <ColorPickerPopover
                    key={field}
                    label={label}
                    value={text[field] as string | null}
                    defaultColor={defaultHex}
                    compact
                    onChange={(v) => updateText({ [field]: v } as Partial<TerminalTextPreferences>)}
                  />
                ))}
              </div>
            </div>
          </div>
        )}

        {tab === "cursor" && (
          <div className="modal-tab-panel">
            <div className="modal-field">
              <label className="modal-label">Active shape</label>
              <SelectPopover
                label="Active cursor shape"
                value={cursor.active_shape}
                options={ACTIVE_SHAPE_OPTIONS}
                onChange={(v) => updateCursor({ active_shape: v })}
              />
            </div>

            <div className="modal-field">
              <label className="modal-label">Inactive shape</label>
              <SelectPopover
                label="Inactive cursor shape"
                value={cursor.inactive_shape}
                options={INACTIVE_SHAPE_OPTIONS}
                onChange={(v) => updateCursor({ inactive_shape: v })}
              />
            </div>

            <label className="modal-checkbox-row">
              <input
                type="checkbox"
                checked={cursor.blink}
                onChange={(e) => updateCursor({ blink: e.target.checked })}
              />
              Blinking cursor
            </label>

            <div className="modal-field">
              <label className="modal-label">Colors</label>
              <div className="color-picker-row">
                <ColorPickerPopover
                  label="Cursor"
                  value={cursor.color}
                  defaultColor={defaultCursorColor}
                  onChange={(v) => updateCursor({ color: v })}
                />
                <ColorPickerPopover
                  label="Accent"
                  value={cursor.accent_color}
                  defaultColor={defaultCursorAccentColor}
                  onChange={(v) => updateCursor({ accent_color: v })}
                />
              </div>
            </div>
          </div>
        )}

        {tab === "compatibility" && (
          <div className="modal-tab-panel">
            <div className="modal-field">
              <label className="modal-label">Backspace key</label>
              <SelectPopover
                label="Backspace key"
                value={compatibility.backspace_key}
                options={KEY_ACTION_OPTIONS}
                onChange={(v) => updateCompatibility({ backspace_key: v })}
              />
            </div>

            <div className="modal-field">
              <label className="modal-label">Delete key</label>
              <SelectPopover
                label="Delete key"
                value={compatibility.delete_key}
                options={KEY_ACTION_OPTIONS}
                onChange={(v) => updateCompatibility({ delete_key: v })}
              />
            </div>
          </div>
        )}

        <div className="modal-buttons">
          <button className="modal-btn-action modal-btn-cancel" onClick={onClose} disabled={submitting}>
            Cancel
          </button>
          <button className="modal-btn-action modal-btn-ok" onClick={commit} disabled={submitting}>
            Save
          </button>
        </div>
      </div>
    </div>
  );
}
