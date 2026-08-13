import { createContext, useCallback, useContext, useEffect, useMemo, useRef, ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";

/** What the native Edit menu's Cut/Copy/Paste items can currently do, and how to do it. */
interface EditMenuHandler {
  canCut: boolean;
  canCopy: boolean;
  canPaste: boolean;
  cut?: () => void;
  copy?: () => void;
  paste?: () => void;
}

interface EditMenuContextValue {
  /**
   * Registers `build`, called on every recompute ahead of the generic
   * `<input>`/selection fallback below — `build` should return `null`
   * whenever its surface isn't the relevant target (e.g. not focused), so
   * the fallback can take over. Only one override is tracked at a time,
   * since only Terminal (whose selection/paste model xterm owns, invisible
   * to both `document.activeElement` and `window.getSelection()`) needs one
   * today. Returns an unregister function.
   */
  registerOverride: (build: () => EditMenuHandler | null) => () => void;
  /** Forces an immediate recompute, for override state changes that don't fire a DOM focus/selection event (e.g. xterm's `onSelectionChange`). */
  notifyChanged: () => void;
}

const EditMenuContext = createContext<EditMenuContextValue | null>(null);

/** Input types that support `selectionStart`/`setSelectionRange` — others (number, checkbox, color, range, etc.) throw if queried. */
const TEXT_INPUT_TYPES = new Set(["text", "search", "url", "tel", "password"]);

function isPlainEditable(el: Element | null): el is HTMLInputElement | HTMLTextAreaElement {
  if (el instanceof HTMLInputElement) return TEXT_INPUT_TYPES.has(el.type) && !el.disabled && !el.readOnly;
  if (el instanceof HTMLTextAreaElement) return !el.disabled && !el.readOnly;
  return false;
}

/** Replaces `el`'s value via React's native-value setter so its controlled `onChange` still fires, then dispatches the bubbling `input` event React listens for. */
function setNativeValue(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  setter?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/**
 * Default Cut/Copy/Paste handling for a focused plain `<input>`/`<textarea>`
 * — covers every ordinary form field across the app's dialogs and panels
 * without any per-field opt-in, using the clipboard-manager plugin rather
 * than `execCommand` (script-triggered paste is blocked by the webview the
 * same as any other browser content; a native menu click is script-driven
 * from here, not a trusted key event).
 */
function defaultHandlerFor(el: HTMLInputElement | HTMLTextAreaElement): EditMenuHandler {
  const hasSelection = el.selectionStart !== el.selectionEnd;
  const replaceSelection = (text: string) => {
    const start = el.selectionStart ?? el.value.length;
    const end = el.selectionEnd ?? el.value.length;
    setNativeValue(el, el.value.slice(0, start) + text + el.value.slice(end));
    const caret = start + text.length;
    el.setSelectionRange(caret, caret);
  };
  const selectedText = () => el.value.slice(el.selectionStart ?? 0, el.selectionEnd ?? 0);
  return {
    canCut: hasSelection,
    canCopy: hasSelection,
    canPaste: true,
    cut: () => {
      const text = selectedText();
      if (!text) return;
      writeText(text).then(() => replaceSelection("")).catch((err) => console.error("cut failed:", err));
    },
    copy: () => {
      const text = selectedText();
      if (text) writeText(text).catch((err) => console.error("copy failed:", err));
    },
    paste: () => {
      readText().then((text) => { if (text) replaceSelection(text); }).catch((err) => console.error("paste failed:", err));
    },
  };
}

/**
 * Copy-only fallback for read-only but selectable content (Trace, Log,
 * Watchpoints, etc.) — none of those panels render to canvas or set
 * `user-select: none`, so a plain DOM text selection already reflects what
 * the user dragged over; no per-panel registration needed.
 */
function selectionHandler(): EditMenuHandler | null {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.toString() === "") return null;
  return {
    canCut: false,
    canCopy: true,
    canPaste: false,
    copy: () => {
      const text = window.getSelection()?.toString();
      if (text) writeText(text).catch((err) => console.error("copy failed:", err));
    },
  };
}

/**
 * Populates the native Edit menu (Cut/Copy/Paste) and tracks its enabled
 * state against wherever focus/selection currently is in the main window —
 * the only window carrying the app menu. Mounted once, wrapping both of
 * `App.tsx`'s return blocks so dialogs shown before the session is ready
 * (New Profile, etc.) are covered too, not just the dock layout.
 */
export function EditMenuProvider({ children }: { children: ReactNode }) {
  const overrideRef = useRef<(() => EditMenuHandler | null) | null>(null);
  const activeHandlerRef = useRef<EditMenuHandler | null>(null);
  const lastPushedRef = useRef<string | null>(null);

  const recompute = useCallback(() => {
    const handler =
      overrideRef.current?.() ??
      (isPlainEditable(document.activeElement) ? defaultHandlerFor(document.activeElement) : null) ??
      selectionHandler();
    activeHandlerRef.current = handler;

    const flags = { cut: handler?.canCut ?? false, copy: handler?.canCopy ?? false, paste: handler?.canPaste ?? false };
    const key = JSON.stringify(flags);
    if (key === lastPushedRef.current) return;
    lastPushedRef.current = key;
    invoke("set_edit_menu_enabled", { flags }).catch((err) => console.error("set_edit_menu_enabled failed:", err));
  }, []);

  const registerOverride = useCallback((build: () => EditMenuHandler | null) => {
    overrideRef.current = build;
    recompute();
    return () => {
      if (overrideRef.current === build) {
        overrideRef.current = null;
        recompute();
      }
    };
  }, [recompute]);

  useEffect(() => {
    document.addEventListener("focusin", recompute);
    document.addEventListener("focusout", recompute);
    document.addEventListener("selectionchange", recompute);
    recompute();
    return () => {
      document.removeEventListener("focusin", recompute);
      document.removeEventListener("focusout", recompute);
      document.removeEventListener("selectionchange", recompute);
    };
  }, [recompute]);

  // A native Edit-menu click (see `on_menu_event` in `lib.rs`) reaches this
  // handler as an event targeted at the main window, the same way the Run
  // and Memory menus dispatch — acted on via whatever's most recently been
  // computed as the active target, since (unlike those menus) there's no
  // single owning panel to route to.
  useEffect(() => {
    const unlistenPromise = listen<string>("edit-menu-action", (event) => {
      const handler = activeHandlerRef.current;
      if (event.payload === "cut") handler?.cut?.();
      else if (event.payload === "copy") handler?.copy?.();
      else if (event.payload === "paste") handler?.paste?.();
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const value = useMemo(() => ({ registerOverride, notifyChanged: recompute }), [registerOverride, recompute]);

  return <EditMenuContext.Provider value={value}>{children}</EditMenuContext.Provider>;
}

/** Returns the Edit menu override API, or `null` outside an `EditMenuProvider` — e.g. `TerminalPanel` also mounts in the detached-Terminal window, which carries no app menu (see `remove_menu` in `lib.rs`). */
export function useEditMenuOverride(): EditMenuContextValue | null {
  return useContext(EditMenuContext);
}
