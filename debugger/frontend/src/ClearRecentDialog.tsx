import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/profile.scss";

/**
 * Confirmation modal for File > Open Recent > Clear Recent…. Only mounted in
 * the main window's `App.tsx` — Open Recent (like New Profile and Open
 * Profile) is a main-window-only menu item, opened via the
 * `open-clear-recent-dialog` Tauri event emitted from `on_menu_event`.
 */
export default function ClearRecentDialog() {
  const [open, setOpen] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    const unlistenPromise = listen("open-clear-recent-dialog", () => {
      setOpen(true);
      setSubmitting(false);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const cancel = () => setOpen(false);

  const commit = async () => {
    if (submitting) return;
    setSubmitting(true);
    try {
      await invoke("clear_recent_profiles");
      setOpen(false);
    } catch (e) {
      console.error("clear_recent_profiles failed:", e);
      setSubmitting(false);
    }
  };

  /** Esc cancels, Enter confirms, while the dialog is open. */
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); cancel(); }
      if (e.key === "Enter") { e.preventDefault(); commit(); }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [open, submitting]);

  if (!open) return null;

  return (
    <div className="new-profile-backdrop" onClick={cancel}>
      <div className="new-profile-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="new-profile-title">Clear Recent</div>

        <div className="new-profile-message">
          Remove all profiles from the Open Recent list? This does not delete any profile files.
        </div>

        <div className="new-profile-buttons">
          <button className="new-profile-btn-action new-profile-btn-cancel" onClick={cancel} disabled={submitting}>
            Cancel
          </button>
          <button className="new-profile-btn-action new-profile-btn-ok" onClick={commit} disabled={submitting}>
            Clear
          </button>
        </div>
      </div>
    </div>
  );
}
