import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/modal.scss";

/** One bundled starter-profile template, as returned by `list_templates`. */
interface TemplateInfo {
  /** Stable identifier passed back to `create_profile` as `template`. */
  id: string;
  /** Human-readable name shown in the picker. */
  name: string;
  /** One-line description shown alongside `name` in the picker. */
  description: string;
}

/** State for the New Profile dialog; null means closed. */
interface DialogState {
  /** Controlled value of the name input. */
  name: string;
  /** Id of the currently-selected starter template. */
  templateId: string;
  /** Starter templates available to pick from, fetched when the dialog opens. */
  templates: TemplateInfo[];
  /** Validation or backend error message; empty string means no error. */
  error: string;
  /** True while `create_profile` is in flight, to disable the form. */
  submitting: boolean;
}

/**
 * Modal for File > New Profile / Ctrl+N. This component is only mounted in
 * the main window's `App.tsx`, so both open paths are scoped there: the
 * native menu item's click reaches it via the `open-new-profile-dialog`
 * Tauri event emitted from `on_menu_event`, and Ctrl+N is handled locally
 * below rather than through the cross-window `APP_KEY_BINDINGS` array (which
 * would also fire it from the Terminal/Trace windows).
 */
export default function NewProfileDialog() {
  const [dialog, setDialog] = useState<DialogState | null>(null);

  /** Fetches the available templates and opens the dialog with the first one selected. */
  const openDialog = async () => {
    const templates = await invoke<TemplateInfo[]>("list_templates");
    setDialog({ name: "", templateId: templates[0]?.id ?? "default", templates, error: "", submitting: false });
  };

  useEffect(() => {
    const unlistenPromise = listen("open-new-profile-dialog", () => { openDialog(); });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  /** Ctrl+N: open the dialog, scoped to this (main) window only. */
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "n" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        setDialog((d) => {
          if (!d) openDialog();
          return d;
        });
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  /** Dismiss the dialog on Escape while it is open. */
  useEffect(() => {
    if (!dialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [dialog]);

  /** Validates the name, invokes create_profile, and closes on success. */
  const commit = async () => {
    if (!dialog || dialog.submitting) return;
    const name = dialog.name.trim();
    if (!name) {
      setDialog((d) => d && { ...d, error: "Enter a profile name" });
      return;
    }
    setDialog((d) => d && { ...d, submitting: true, error: "" });
    try {
      await invoke("create_profile", { name, template: dialog.templateId });
      setDialog(null);
    } catch (e) {
      setDialog((d) => d && { ...d, error: String(e), submitting: false });
    }
  };

  if (!dialog) return null;

  return (
    <div className="modal-backdrop" onClick={() => setDialog(null)}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">New Profile</div>

        <div className="modal-field">
          <label className="modal-label">Name</label>
          <input
            className={`modal-input${dialog.error ? " invalid" : ""}`}
            autoFocus
            spellCheck={false}
            placeholder="profile name"
            value={dialog.name}
            disabled={dialog.submitting}
            onChange={(e) => setDialog((d) => d && { ...d, name: e.target.value, error: "" })}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") { e.preventDefault(); commit(); }
              if (e.key === "Escape") { e.preventDefault(); setDialog(null); }
            }}
          />
        </div>

        <div className="modal-field modal-template-field">
          <label className="modal-label">Template</label>
          <div className="modal-template-list" role="radiogroup" aria-label="Starter template">
            {dialog.templates.map((t) => (
              <label key={t.id} className="modal-template-row">
                <input
                  type="radio"
                  name="template"
                  checked={dialog.templateId === t.id}
                  disabled={dialog.submitting}
                  onChange={() => setDialog((d) => d && { ...d, templateId: t.id })}
                />
                <div className="modal-template-text">
                  <span className="modal-template-name">{t.name}</span>
                  <span className="modal-template-desc">{t.description}</span>
                </div>
              </label>
            ))}
          </div>
        </div>

        {dialog.error && <div className="modal-error">{dialog.error}</div>}

        <div className="modal-buttons">
          <button
            className="modal-btn-action modal-btn-cancel"
            onClick={() => setDialog(null)}
            disabled={dialog.submitting}
          >
            Cancel
          </button>
          <button
            className="modal-btn-action modal-btn-ok"
            onClick={commit}
            disabled={dialog.submitting}
          >
            OK
          </button>
        </div>
      </div>
    </div>
  );
}
