import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/modal.scss";

/**
 * Confirmation modal for Window > Restore Layout… (issue #398). Only mounted
 * in the main window's `App.tsx` — Window is a main-window-only menu, opened
 * via the `open-restore-layout-dialog` Tauri event emitted from
 * `on_menu_event`. Committing invokes `restore_dock_layout`, which discards
 * the persisted arrangement and tells `DockLayout.tsx` (via the
 * `dock-layout-reset` event) to rebuild the default one.
 */
export default function RestoreLayoutDialog() {
  const [open, setOpen] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    const unlistenPromise = listen("open-restore-layout-dialog", () => {
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
      await invoke("restore_dock_layout");
      setOpen(false);
    } catch (e) {
      console.error("restore_dock_layout failed:", e);
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
    <div className="modal-backdrop" onClick={cancel}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">Restore Layout</div>

        <div className="modal-message">
          Restore all panels to their default locations and sizes? This cannot be undone.
        </div>

        <div className="modal-buttons">
          <button className="modal-btn-action modal-btn-cancel" onClick={cancel} disabled={submitting}>
            Cancel
          </button>
          <button className="modal-btn-action modal-btn-ok" onClick={commit} disabled={submitting}>
            Restore
          </button>
        </div>
      </div>
    </div>
  );
}
