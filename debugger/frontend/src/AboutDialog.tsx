import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/modal.scss";

interface AboutInfo {
  build_info: string | null;
}

/**
 * Help > About dialog (issue #423): static app name/description/copyright/
 * license text, plus a build-info line (git commit hash, build timestamp)
 * fetched from `get_about_info` — present only in production builds, `null`
 * in dev (see `about.rs`). Opened via the `open-about-dialog` event emitted
 * from `on_menu_event`, mirroring `RestoreLayoutDialog`/`ExitConfirmDialog`.
 */
export default function AboutDialog() {
  const [open, setOpen] = useState(false);
  const [buildInfo, setBuildInfo] = useState<string | null>(null);

  useEffect(() => {
    const unlistenPromise = listen("open-about-dialog", () => {
      setOpen(true);
      invoke<AboutInfo>("get_about_info")
        .then((info) => setBuildInfo(info.build_info))
        .catch((e) => console.error("get_about_info failed:", e));
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const close = () => setOpen(false);

  /** Esc or Enter both dismiss — there's nothing to confirm. */
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape" || e.key === "Enter") { e.preventDefault(); close(); }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [open]);

  if (!open) return null;

  return (
    <div className="modal-backdrop" onClick={close}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">About</div>

        <div className="modal-message">
          Emma65: Emulator and Debugger for the 65C02 series microprocessor and related peripherals
        </div>

        <div className="modal-message">
          Copyright (c) 2026 Carl Harris, Jr.
          <br />
          Open-source distribution under the MIT license.
        </div>

        {buildInfo && <div className="modal-label">{buildInfo}</div>}

        <div className="modal-buttons">
          <button className="modal-btn-action modal-btn-ok" onClick={close}>
            OK
          </button>
        </div>
      </div>
    </div>
  );
}
