import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
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

/**
 * Registers `id`'s single tab-header action, replacing it on every change and clearing it on
 * unmount. `action.onClick` is read through a ref rather than listed in the effect's dependency
 * array: callers idiomatically pass an inline `{ ..., onClick: () => ... }` object literal, which
 * is a new function identity on every render — depending on it directly would re-run this effect,
 * and thus `register`'s provider-state update, on every render of every panel using this hook,
 * which (since the provider re-render fans back out to every consumer) never settles.
 */
export function usePanelHeaderAction(id: MainPanelId, action: PanelHeaderAction) {
  const ctx = useContext(PanelHeaderActionContext);
  if (!ctx) throw new Error("usePanelHeaderAction must be used within a PanelHeaderActionProvider");
  const { register } = ctx;
  const onClickRef = useRef(action.onClick);
  onClickRef.current = action.onClick;
  useEffect(() => {
    register(id, { ...action, onClick: () => onClickRef.current() });
    return () => register(id, null);
  }, [register, id, action.title, action.disabled, action.disabledTitle]);
}

/**
 * Same as `usePanelHeaderAction`, but a no-op outside a `PanelHeaderActionProvider`
 * instead of throwing — for a panel that, like Terminal, also mounts in a host with
 * no provider ancestor (the detached-Terminal window has its own document with no
 * `DockLayout` around it; see `useEditMenuOverride` in `EditMenuContext.tsx` for the
 * same pattern applied to the native Edit menu).
 */
export function useOptionalPanelHeaderAction(id: MainPanelId, action: PanelHeaderAction) {
  const ctx = useContext(PanelHeaderActionContext);
  const register = ctx?.register;
  const onClickRef = useRef(action.onClick);
  onClickRef.current = action.onClick;
  useEffect(() => {
    if (!register) return;
    register(id, { ...action, onClick: () => onClickRef.current() });
    return () => register(id, null);
  }, [register, id, action.title, action.disabled, action.disabledTitle]);
}

/** Reads the current id -> action map, for `DockTabActions` to render from. */
export function usePanelHeaderActions() {
  const ctx = useContext(PanelHeaderActionContext);
  if (!ctx) throw new Error("usePanelHeaderActions must be used within a PanelHeaderActionProvider");
  return ctx.actions;
}
