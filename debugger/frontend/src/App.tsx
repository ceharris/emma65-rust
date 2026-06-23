import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

interface SessionStatus {
  message: string;
  ok: boolean;
}

export default function App() {
  const [status, setStatus] = useState<SessionStatus | null>(null);

  useEffect(() => {
    // Register the event listener before polling so no event can slip through.
    const unlistenPromise = listen<SessionStatus>("session-status", (event) => {
      setStatus(event.payload);
    });

    // Poll for a status that may have already been set before we registered.
    invoke<SessionStatus | null>("get_session_status").then((current) => {
      if (current !== null) {
        setStatus(current);
      }
    });

    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  return (
    <div className="app">
      {status === null ? (
        <span className="status-pending">Initializing...</span>
      ) : (
        <span className={status.ok ? "status-ok" : "status-error"}>
          {status.message}
        </span>
      )}
    </div>
  );
}
