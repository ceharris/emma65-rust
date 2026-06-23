import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

interface SessionStatus {
  message: string;
  ok: boolean;
}

export default function App() {
  const [status, setStatus] = useState<SessionStatus | null>(null);

  useEffect(() => {
    const unlisten = listen<SessionStatus>("session-status", (event) => {
      setStatus(event.payload);
    });
    return () => { unlisten.then((f) => f()); };
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
