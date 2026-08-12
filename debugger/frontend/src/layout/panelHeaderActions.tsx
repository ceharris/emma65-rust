import { createContext, useCallback, useContext, useEffect, useState } from "react";
import { MainPanelId } from "./panelRegistry";

/** A single tab-header action button a panel wants dockview to render in its tab bar. */
export interface PanelHeaderAction {
  title: string;
  onClick: () => void;
  disabled?: boolean;
  disabledTitle?: string;
}

interface PanelHeaderActionContextValue {
  actions: Partial<Record<MainPanelId, PanelHeaderAction>>;
  register: (id: MainPanelId, action: PanelHeaderAction | null) => void;
}

const PanelHeaderActionContext = createContext<PanelHeaderActionContextValue | null>(null);

/**
 * Generalizes the pattern `DockLayout.tsx`'s `DockTabActions` already used for
 * Terminal's Detach and Run Controls' Float buttons: a panel registers a
 * single `{title, onClick}` action here instead of building its own in-body
 * header row, and `DockTabActions` renders whatever's registered for the
 * active tab's panel id.
 */
export function PanelHeaderActionProvider({ children }: { children: React.ReactNode }) {
  const [actions, setActions] = useState<Partial<Record<MainPanelId, PanelHeaderAction>>>({});
  const register = useCallback((id: MainPanelId, action: PanelHeaderAction | null) => {
    setActions((prev) => {
      if (action === null) {
        if (!(id in prev)) return prev;
        const next = { ...prev };
        delete next[id];
        return next;
      }
      return { ...prev, [id]: action };
    });
  }, []);
  return <PanelHeaderActionContext.Provider value={{ actions, register }}>{children}</PanelHeaderActionContext.Provider>;
}

/** Registers `id`'s single tab-header action, replacing it on every change and clearing it on unmount. */
export function usePanelHeaderAction(id: MainPanelId, action: PanelHeaderAction) {
  const ctx = useContext(PanelHeaderActionContext);
  if (!ctx) throw new Error("usePanelHeaderAction must be used within a PanelHeaderActionProvider");
  const { register } = ctx;
  useEffect(() => {
    register(id, action);
    return () => register(id, null);
  }, [register, id, action.title, action.onClick, action.disabled, action.disabledTitle]);
}

/** Reads the current id -> action map, for `DockTabActions` to render from. */
export function usePanelHeaderActions() {
  const ctx = useContext(PanelHeaderActionContext);
  if (!ctx) throw new Error("usePanelHeaderActions must be used within a PanelHeaderActionProvider");
  return ctx.actions;
}
