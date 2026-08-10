import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/profile.scss";

/** State for the New Profile dialog; null means closed. */
interface DialogState {
  /** Controlled value of the name input. */
  name: string;
  /** Validation or backend error message; empty string means no error. */
  error: string;
  /** True while `create_profile` is in flight, to disable the form. */
  submitting: boolean;
}

/**
 * Modal for File > New Profile / Ctrl+N. Opens on the `open-new-profile-dialog`
 * event, which both the native menu item (via `on_menu_event`) and the Ctrl+N
 * key binding (via the `open_new_profile_dialog` command) funnel through, so
 * the dialog opens the same way regardless of which window had focus.
 */
export default function NewProfileDialog() {
  const [dialog, setDialog] = useState<DialogState | null>(null);

  useEffect(() => {
    const unlistenPromise = listen("open-new-profile-dialog", () => {
      setDialog({ name: "", error: "", submitting: false });
    });
    return () => { unlistenPromise.then((f) => f()); };
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
      await invoke("create_profile", { name });
      setDialog(null);
    } catch (e) {
      setDialog((d) => d && { ...d, error: String(e), submitting: false });
    }
  };

  if (!dialog) return null;

  return (
    <div className="new-profile-backdrop" onClick={() => setDialog(null)}>
      <div className="new-profile-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="new-profile-title">New Profile</div>

        <div className="new-profile-field">
          <label className="new-profile-label">Name</label>
          <input
            className={`new-profile-input${dialog.error ? " invalid" : ""}`}
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

        {dialog.error && <div className="new-profile-error">{dialog.error}</div>}

        <div className="new-profile-buttons">
          <button
            className="new-profile-btn-action new-profile-btn-cancel"
            onClick={() => setDialog(null)}
            disabled={dialog.submitting}
          >
            Cancel
          </button>
          <button
            className="new-profile-btn-action new-profile-btn-ok"
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
