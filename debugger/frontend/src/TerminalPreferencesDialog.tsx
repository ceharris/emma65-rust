import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./styles/modal.scss";
import { isMonospaceFont } from "./terminalSizing";
import ColorPickerPopover from "./ColorPickerPopover";
import { ANSI_PALETTE_FIELDS, TerminalPreferences, TerminalTextPreferences } from "./terminalPreferences";

interface TerminalPreferencesDialogProps {
  open: boolean;
  onClose: () => void;
  /**
   * Called with the saved preferences right after a successful
   * `set_terminal_preferences`, so the caller (`TerminalPanel`) can apply
   * the Text-tab-relevant values to the already-open live terminal.
   */
  onSaved: (preferences: TerminalPreferences) => void;
}

/**
 * Only "text" exists so far — Work Units 3/4 add "cursor"/"compatibility"
 * entries here (and a corresponding tab-panel branch below) without needing
 * to touch the tab-strip rendering itself.
 */
type TabId = "text";
const TABS: { id: TabId; label: string }[] = [{ id: "text", label: "Text" }];

/**
 * The terminal's "Preferences…" modal (issue #467), opened from
 * `TerminalPanel.tsx`'s size-preset menu. Follows the existing dialog
 * pattern (`AboutDialog.tsx`/`NewProfileDialog.tsx`: local state, manual
 * Escape/Enter handling, `modal.scss` classes) since there's no shared
 * `<Modal>` component. Mounted directly by `TerminalPanel` (not globally in
 * `App.tsx` like `AboutDialog`) since it needs `open`/`onSaved` wired to
 * whichever terminal instance — docked or detached — actually opened it.
 */
export default function TerminalPreferencesDialog({ open, onClose, onSaved }: TerminalPreferencesDialogProps) {
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

  const { text } = preferences;

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
              <input
                type="number"
                className="modal-input modal-input-narrow"
                min={6}
                max={72}
                placeholder="default"
                value={text.font_size ?? ""}
                onChange={(e) => updateText({ font_size: e.target.value ? Number(e.target.value) : null })}
              />
            </div>

            <div className="modal-field">
              <label className="modal-label">Scrollback</label>
              <input
                type="number"
                className="modal-input modal-input-narrow"
                min={0}
                max={100000}
                value={text.scrollback}
                onChange={(e) => updateText({ scrollback: Math.max(0, Number(e.target.value) || 0) })}
              />
            </div>

            <div className="modal-field">
              <label className="modal-label">Colors</label>
              <div className="color-picker-row">
                <ColorPickerPopover label="Foreground" value={text.foreground} onChange={(v) => updateText({ foreground: v })} />
                <ColorPickerPopover label="Background" value={text.background} onChange={(v) => updateText({ background: v })} />
              </div>
            </div>

            <div className="modal-field modal-palette-field">
              <label className="modal-label">Palette</label>
              <div className="color-picker-grid">
                {ANSI_PALETTE_FIELDS.map(({ field, label }) => (
                  <ColorPickerPopover
                    key={field}
                    label={label}
                    value={text[field] as string | null}
                    onChange={(v) => updateText({ [field]: v } as Partial<TerminalTextPreferences>)}
                  />
                ))}
              </div>
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
