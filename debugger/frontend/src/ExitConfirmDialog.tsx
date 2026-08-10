import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/profile.scss";

/**
 * Confirmation modal for exiting the debugger: File > Exit, Ctrl+Q (bound
 * app-wide, so it can arrive here even when the Terminal or Trace window had
 * focus), and closing the main window via the window manager. Only mounted
 * in the main window's `App.tsx` — the backend's `request_exit` focuses this
 * window before emitting `open-exit-confirm-dialog`, mirroring
 * `NewProfileDialog`/`ClearRecentDialog`.
 *
 * Canceling never invokes the backend — the dialog just closes locally,
 * leaving the "Don't ask again" preference and the process untouched.
 * Committing always invokes `confirm_exit`, which persists the checkbox
 * state and exits.
 */
export default function ExitConfirmDialog() {
  const [open, setOpen] = useState(false);
  const [dontAskAgain, setDontAskAgain] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    const unlistenPromise = listen("open-exit-confirm-dialog", () => {
      setOpen(true);
      setDontAskAgain(false);
      setSubmitting(false);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const cancel = () => setOpen(false);

  const commit = async () => {
    if (submitting) return;
    setSubmitting(true);
    try {
      await invoke("confirm_exit", { skipConfirmation: dontAskAgain });
      // No further UI update expected: a successful call exits the process.
    } catch (e) {
      console.error("confirm_exit failed:", e);
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
  }, [open, submitting, dontAskAgain]);

  if (!open) return null;

  return (
    <div className="new-profile-backdrop" onClick={cancel}>
      <div className="new-profile-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="new-profile-title">Exit</div>

        <div className="new-profile-message">
          Are you sure you want to exit the debugger?
        </div>

        <label className="new-profile-checkbox-row">
          <input
            type="checkbox"
            checked={dontAskAgain}
            onChange={(e) => setDontAskAgain(e.target.checked)}
            disabled={submitting}
          />
          Don't ask again
        </label>

        <div className="new-profile-buttons">
          <button className="new-profile-btn-action new-profile-btn-cancel" onClick={cancel} disabled={submitting}>
            Cancel
          </button>
          <button className="new-profile-btn-action new-profile-btn-ok" onClick={commit} disabled={submitting}>
            Exit
          </button>
        </div>
      </div>
    </div>
  );
}
