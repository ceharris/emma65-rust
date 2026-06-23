import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import DisassemblyPanel from "./DisassemblyPanel";
import RegisterPanel from "./RegisterPanel";

interface SessionStatus {
  message: string;
  ok: boolean;
}

export default function App() {
  const [status, setStatus] = useState<SessionStatus | null>(null);
  const [stepCount, setStepCount] = useState(0);

  useEffect(() => {
    const unlistenPromise = listen<SessionStatus>("session-status", (event) => {
      setStatus(event.payload);
    });

    invoke<SessionStatus | null>("get_session_status").then((current) => {
      if (current !== null) {
        setStatus(current);
      }
    });

    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const handleStep = useCallback(() => {
    setStepCount((n) => n + 1);
  }, []);

  if (status === null || !status.ok) {
    return (
      <div className="app-splash">
        {status === null ? (
          <span className="status-pending">Initializing…</span>
        ) : (
          <span className="status-error">{status.message}</span>
        )}
      </div>
    );
  }

  return (
    <div className="app-layout">
      <div className="col col-left">
        {/* Memory view placeholder — story 5 */}
      </div>
      <div className="col col-center">
        <DisassemblyPanel onStep={handleStep} />
      </div>
      <div className="col col-right">
        <RegisterPanel refreshKey={stepCount} />
        {/* Stack view placeholder — story 6 */}
        {/* Watchpoints placeholder — story 12 */}
      </div>
    </div>
  );
}
