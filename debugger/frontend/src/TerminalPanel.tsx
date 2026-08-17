import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { DockviewPanelApi } from "dockview-react";
import { Terminal, ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import "@xterm/xterm/css/xterm.css";
import { APP_KEY_BINDINGS } from "./useAppKeyBindings";
import { useEditMenuOverride } from "./EditMenuContext";
import { useTheme } from "./ThemeContext";
import { useOptionalPanelHeaderAction } from "./layout/panelHeaderActions";
import { resolveTerminalFont, pixelSizeForGrid, logicalSizeForCssPixels, TERMINAL_SIZE_PRESETS } from "./terminalSizing";
import {
  DEFAULT_CURSOR_ACCENT_COLOR,
  terminalKeyActionBytes,
  themeWithCursorOverrides,
  themeWithTextOverrides,
  TerminalCompatibilityPreferences,
  TerminalCursorPreferences,
  TerminalPreferences,
  TerminalTextPreferences,
} from "./terminalPreferences";
import TerminalPreferencesDialog from "./TerminalPreferencesDialog";

const XTERM_DARK_THEME: ITheme = {
  background: "#1e1e1e",
  foreground: "#d4d4d4",
  cursor: "#d4d4d4",
  // xterm falls back to a default that isn't visible against every
  // background it ships with, so this must be set explicitly — matches
  // $dark-palette's bg-selected in global.scss.
  selectionBackground: "#094771",
};

const XTERM_LIGHT_THEME: ITheme = {
  background: "#ffffff",
  foreground: "#1e1e1e",
  cursor: "#1e1e1e",
  // See XTERM_DARK_THEME — matches $light-palette's bg-selected.
  selectionBackground: "#cce4f7",
};

/** The platform default monospace font, from the `--font-mono` CSS var. */
function getFallbackMonoFont(): string {
  return getComputedStyle(document.documentElement).getPropertyValue("--font-mono").trim() || "monospace";
}

/**
 * The dock panel hosting the emulator's console. Shaped for reuse: it reads
 * the theme via `useTheme()` rather than tracking it independently, so both
 * the main window's docked instance and the detached-window host
 * (`terminal-detached.tsx`, issue #385) just need their own `ThemeProvider`
 * ancestor, the same way `main.tsx` wraps `App`. Global key bindings
 * (`useAppKeyBindings`) are installed by the host document, not here — the
 * main window installs them once at `App.tsx`'s root, and
 * `terminal-detached.tsx` does the same for its own window.
 *
 * Detach/reattach (`DockLayout.tsx`'s `closeTerminalPanel`/`terminal-reattached`
 * handler) always closes this component's dock panel and later re-`addPanel`s
 * a brand new one, rather than reparenting an existing instance — so every
 * detach/reattach cycle fully unmounts and remounts this component, tearing
 * down its `Terminal` and rebuilding one from scratch against whichever host
 * (docked panel or detached window) it lands in next. Confirmed by issue
 * #462's Work Unit 3 audit: this is what makes a freshly remounted
 * terminal's grid always match its *current* host's actual size, with no way
 * for a size measured against a previous host to leak into the next one.
 */
interface TerminalPanelProps {
  /**
   * The dockview panel API for this component's own dock panel, threaded
   * down by `panelRegistry.tsx`. `undefined` when this component is mounted
   * in the detached-Terminal window (`terminal-detached.tsx`), which has no
   * dockview instance at all — the size-preset menu (issue #462 Work Unit 4)
   * uses its presence/absence to decide whether resizing a preset means
   * `dockPanelApi.setSize()` (docked) or `getCurrentWindow().setSize()`
   * (detached).
   */
  dockPanelApi?: DockviewPanelApi;
}

export default function TerminalPanel({ dockPanelApi }: TerminalPanelProps = {}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  // Holds the Text-tab/Cursor-tab preferences currently applied to the live
  // terminal, so the theme-sync effect below can recompute overrides on a
  // light/dark switch without needing its own separate fetch.
  const textPreferencesRef = useRef<TerminalTextPreferences | null>(null);
  const cursorPreferencesRef = useRef<TerminalCursorPreferences | null>(null);
  // Holds the Compatibility-tab preferences currently applied — read
  // directly by the construction effect's `attachCustomKeyEventHandler`
  // below, since there's no live xterm option for Backspace/Delete
  // behavior; a ref rather than state so a saved change is picked up
  // without re-running that effect (which would tear down and rebuild the
  // whole terminal).
  const compatibilityPreferencesRef = useRef<TerminalCompatibilityPreferences | null>(null);
  const { resolvedTheme } = useTheme();
  const editMenu = useEditMenuOverride();
  const [preferencesOpen, setPreferencesOpen] = useState(false);
  // Position of the open size-preset menu — `null` when closed. Hand-drawn
  // (issue #472) rather than a native `Menu.new()`/`popup()` call so it
  // picks up the app's light/dark theme, the same fix shape as
  // `DisassemblyPanel.tsx`'s row context menu, whose `.context-menu`/
  // `-item` CSS this reuses (lifted into global.scss for that reason).
  const [sizeMenu, setSizeMenu] = useState<{ x: number; y: number } | null>(null);
  const sizeMenuRef = useRef<HTMLDivElement | null>(null);

  // Resizes the terminal to `cols`x`rows` by resizing its *host* (the dock
  // panel, or the detached OS window) rather than the `Terminal` instance
  // directly — the existing `ResizeObserver` in the construction effect
  // below then fits the terminal to whatever size the host actually lands
  // on, the same as any other resize. Docked, `pixelSizeForGrid`'s CSS-px
  // result is exactly what `setSize` wants (issue #462's Work Unit 2 doc
  // comment on `logicalSizeForCssPixels`: the docked path stays entirely in
  // CSS/logical px within this webview). Detached, it's converted to a
  // Tauri `LogicalSize` first, honoring the `--scale-factor` CLI override
  // when one was given.
  const resizeToGrid = async (cols: number, rows: number) => {
    const term = termRef.current;
    if (!term) return;
    const size = pixelSizeForGrid(term, cols, rows);
    if (!size) return;
    if (dockPanelApi) {
      // `dockPanelApi.setSize()` forwards to the *group's* setSize
      // (dockview-core's `DockviewPanel` constructor wires
      // `panel.api.onDidSizeChange` straight to `this.group.api.setSize`),
      // i.e. it resizes the whole tab group Terminal is tabbed into —
      // header/tab-strip included — not just this panel's own content box.
      // `dockPanelApi.height` (read) is the content-box height (what this
      // panel's `layout()` actually receives, post-header), so the gap
      // between it and the group's own total height is exactly the
      // tab-strip's height, which has to be added back in or the requested
      // grid comes up short by however many rows that strip is tall.
      const headerOverhead = dockPanelApi.group.height - dockPanelApi.height;
      dockPanelApi.setSize({ width: size.width, height: size.height + headerOverhead });
      return;
    }
    let scaleFactorOverride: number | null = null;
    try {
      scaleFactorOverride = await invoke<number | null>("get_terminal_scale_factor_override");
    } catch (err) {
      console.error("get_terminal_scale_factor_override failed:", err);
    }
    const logical = await logicalSizeForCssPixels(size.width, size.height, scaleFactorOverride);
    await getCurrentWindow().setSize(logical);
  };

  // Opens the size-preset menu at a given viewport position — shared by the
  // terminal container's own `contextmenu` handler (registered in the
  // construction effect below) and the docked panel header's hamburger
  // icon (`usePanelHeaderAction` below, rendered by `DockLayout.tsx`'s
  // `DockTabActions`), both in both docked and detached hosts. The render
  // below reads `termRef.current` fresh on every render, so the checkmark
  // always reflects whichever preset (if any) matches the terminal's
  // *current* grid, same as the native menu this replaced.
  const openSizeMenuAt = (x: number, y: number) => {
    if (!termRef.current) return;
    setSizeMenu({ x, y });
  };
  const closeSizeMenu = () => setSizeMenu(null);

  // Close the size menu on outside click or Escape — same pattern as
  // `DisassemblyPanel.tsx`'s row context menu.
  useEffect(() => {
    if (sizeMenu === null) return;
    const handlePointerDown = (e: MouseEvent) => {
      if (sizeMenuRef.current && !sizeMenuRef.current.contains(e.target as Node)) closeSizeMenu();
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeSizeMenu();
    };
    document.addEventListener("mousedown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [sizeMenu]);

  // No-op outside the docked host (see `useOptionalPanelHeaderAction`'s doc
  // comment) — the detached window has no dock tab header to add an icon
  // to; its size menu is reachable only via right-click. Anchored just
  // below the button, the same way `SelectPopover`'s list anchors under
  // its own trigger.
  useOptionalPanelHeaderAction("terminal", {
    title: "Terminal Size…",
    onClick: (e) => {
      const r = e.currentTarget.getBoundingClientRect();
      openSizeMenuAt(r.left, r.bottom);
    },
  });

  // Recomputes `term.options.theme` from the current light/dark base theme
  // plus whatever Text-tab/Cursor-tab overrides are currently applied
  // (`null` refs — preferences not fetched yet — leave the base theme
  // untouched, same as before either tab existed).
  const applyTheme = () => {
    const term = termRef.current;
    if (!term) return;
    const base = resolvedTheme === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME;
    let theme = textPreferencesRef.current ? themeWithTextOverrides(base, textPreferencesRef.current) : base;
    if (cursorPreferencesRef.current) theme = themeWithCursorOverrides(theme, cursorPreferencesRef.current);
    term.options.theme = theme;
  };

  useEffect(() => {
    applyTheme();
  }, [resolvedTheme]);

  // Applies a fetched/saved Text-tab preferences struct to the
  // already-constructed terminal: scrollback, font, and (via `applyTheme`)
  // the foreground/background/16-palette theme overrides. Used both by the
  // fetch-on-mount effect below and by the Preferences dialog's `onSaved`
  // callback. xterm.js supports changing `options.scrollback`/`fontFamily`/
  // `fontSize`/`theme` on a live instance, so neither caller needs to gate
  // the terminal's initial construction on this running first.
  const applyTextPreferences = (text: TerminalTextPreferences) => {
    const term = termRef.current;
    if (!term) return;
    textPreferencesRef.current = text;
    term.options.scrollback = text.scrollback;
    const resolved = resolveTerminalFont(text.font_family, text.font_size, getFallbackMonoFont());
    term.options.fontFamily = resolved.fontFamily;
    term.options.fontSize = resolved.fontSize;
    applyTheme();
    // A font-metrics change invalidates whatever grid the construction
    // effect's own initial fit() computed against the placeholder font.
    fitAddonRef.current?.fit();
  };

  // Applies a fetched/saved Cursor-tab preferences struct to the
  // already-constructed terminal: shape/blink options plus (via
  // `applyTheme`) the cursor/accent-color theme overrides. Same two callers
  // as `applyTextPreferences` above.
  const applyCursorPreferences = (cursor: TerminalCursorPreferences) => {
    const term = termRef.current;
    if (!term) return;
    cursorPreferencesRef.current = cursor;
    term.options.cursorStyle = cursor.active_shape;
    term.options.cursorInactiveStyle = cursor.inactive_shape;
    term.options.cursorBlink = cursor.blink;
    applyTheme();
  };

  // Stores a fetched/saved Compatibility-tab preferences struct in the ref
  // the construction effect's key handler reads from — nothing to apply to
  // a live xterm option, unlike the Text/Cursor tabs.
  const applyCompatibilityPreferences = (compatibility: TerminalCompatibilityPreferences) => {
    compatibilityPreferencesRef.current = compatibility;
  };

  const fetchAndApplyPreferences = () => {
    invoke<TerminalPreferences>("get_terminal_preferences")
      .then(({ text, cursor, compatibility }) => {
        applyTextPreferences(text);
        applyCursorPreferences(cursor);
        applyCompatibilityPreferences(compatibility);
      })
      .catch((err) => console.error("get_terminal_preferences failed:", err));
  };

  useEffect(() => {
    fetchAndApplyPreferences();
    // Only the initial fetch belongs in this effect — `applyTextPreferences`/
    // `applyCursorPreferences` are also called directly (not via a
    // dependency-triggered re-run) by the Preferences dialog's `onSaved`
    // below.
  }, []);

  // The detached-Terminal window's own `TerminalPanel` mounts exactly once,
  // when its statically-declared hidden webview first loads at app startup
  // (`terminal.rs`'s `install_detached_window`) — unlike the docked panel,
  // it's never unmounted/remounted on later detach/reattach cycles, so the
  // mount-time fetch above only ever reflects preferences as of that one
  // startup moment. `terminal.rs`'s `show_detached_terminal` emits
  // "terminal-shown" to this window specifically every time it's about to
  // become visible, so a preference change made while docked (after this
  // window's one-time mount) still shows up here. Harmless no-op for the
  // docked host — this event is only ever `emit_to`'d to the detached
  // window's label, so the docked instance's listener never fires.
  useEffect(() => {
    const unlistenPromise = listen("terminal-shown", () => fetchAndApplyPreferences());
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  useEffect(() => {
    const term = new Terminal({
      cols: 80,
      rows: 24,
      theme: resolvedTheme === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME,
      fontFamily: getFallbackMonoFont(),
      fontSize: 14,
    });
    termRef.current = term;

    // Shared with the Edit menu's override below (registered further down),
    // so Ctrl+Shift+C/V and a menu click both end up calling the exact same
    // code.
    const copySelection = () => {
      const selection = term.getSelection();
      if (selection) {
        writeText(selection).catch((err) => console.error("copy to clipboard failed:", err));
      }
    };
    const pasteClipboard = () => {
      readText()
        .then((text) => {
          if (text) term.paste(text);
        })
        .catch((err) => console.error("paste from clipboard failed:", err));
    };

    // Let app-wide shortcuts (e.g. Ctrl+Shift+T) bypass xterm's own key
    // handling — otherwise xterm treats them as terminal control input and
    // stops the keydown from ever reaching the window-level listener. Ctrl+Q
    // is deliberately NOT one of these (issue #351): it's scoped to the main
    // window only, so xterm is free to treat it as XON here, same as any
    // other terminal.
    //
    // Ctrl+Shift+C/V are handled here rather than via APP_KEY_BINDINGS because
    // they need direct access to this xterm instance (selection, paste), not
    // just a backend command to invoke.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type === "keydown" && e.ctrlKey && e.shiftKey && e.code === "KeyC") {
        // Without preventDefault, the underlying textarea's native paste/copy
        // key binding (e.g. GTK's own Ctrl+Shift+C/V editing commands) fires
        // in addition to this handler, double-actioning the clipboard.
        e.preventDefault();
        copySelection();
        return false;
      }
      if (e.type === "keydown" && e.ctrlKey && e.shiftKey && e.code === "KeyV") {
        e.preventDefault();
        pasteClipboard();
        return false;
      }
      // Compatibility tab (issue #467 Work Unit 4): sends the configured
      // byte(s) for Backspace/Delete instead of whatever xterm's own default
      // encoding would send, intercepted here (before that default encoding
      // runs) rather than in `onData`, which only ever sees xterm's already
      //-encoded output.
      if (e.type === "keydown" && (e.code === "Backspace" || e.code === "Delete")) {
        const action =
          e.code === "Backspace"
            ? compatibilityPreferencesRef.current?.backspace_key
            : compatibilityPreferencesRef.current?.delete_key;
        if (action) {
          e.preventDefault();
          invoke("write_terminal", { bytes: terminalKeyActionBytes(action) }).catch(() => {});
          return false;
        }
      }
      return !APP_KEY_BINDINGS.some((b) => b.matches(e));
    });

    // Registers an Edit-menu override (issue #435) rather than relying on
    // the generic <input>/<textarea> fallback in `EditMenuContext.tsx`:
    // xterm's hidden `textarea` is a real `HTMLTextAreaElement` but its
    // `.value`/selection range don't reflect the terminal's actual content
    // or selection (xterm owns both internally), and its content is
    // canvas-rendered so `window.getSelection()` can't see it either. `null`
    // outside the main window (the detached-Terminal window has no app
    // menu — see `remove_menu` in `lib.rs`), so this is a no-op there.
    const unregisterOverride = editMenu?.registerOverride(() => {
      if (document.activeElement !== term.textarea) return null;
      return { canCut: false, canCopy: term.hasSelection(), canPaste: true, copy: copySelection, paste: pasteClipboard };
    }) ?? null;
    const selectionChangeDisposable = editMenu ? term.onSelectionChange(() => editMenu.notifyChanged()) : null;

    const fitAddon = new FitAddon();
    fitAddonRef.current = fitAddon;
    term.loadAddon(fitAddon);
    term.open(containerRef.current!);
    fitAddon.fit();
    // Every mount of this component is a moment the user just landed on a
    // terminal they presumably want to type into right away — the very
    // first mount (docked or detached), and every detach/reattach remount
    // (issue #385) — so grab keyboard focus immediately rather than leaving
    // it wherever it happened to be.
    term.focus();

    // The size-preset menu (issue #462 Work Unit 4) — identical in both
    // docked and detached hosts, so a right-click anywhere in the terminal
    // area reaches it the same way regardless of which host this mount
    // landed in. The docked panel additionally gets a header hamburger icon
    // reaching the same menu (see `useOptionalPanelHeaderAction` above).
    const handleContextMenu = (e: MouseEvent) => {
      e.preventDefault();
      openSizeMenuAt(e.clientX, e.clientY);
    };
    containerRef.current!.addEventListener("contextmenu", handleContextMenu);

    // A dockview split drag resizes this panel's container without resizing
    // the OS window, so window-resize alone (the old standalone Terminal
    // window's refit trigger) wouldn't cover it here — this observer handles
    // both split-drag and inactive→active tab transitions. Not started
    // watching until after the history replay below has fully completed —
    // see that block's comment for why.
    //
    // This same observer also covers the detached window's native OS
    // resize, with no separate handler needed (issue #462 Work Unit 3
    // audit): `.terminal-container` is sized to 100% of its document's
    // `html`/`body`/`#root` chain (`global.scss`), which is exactly the
    // detached window's viewport, so an OS-level resize changes this
    // element's observed border box directly.
    const resizeObserver = new ResizeObserver(() => fitAddon.fit());

    term.onData((data) => {
      const bytes = Array.from(new TextEncoder().encode(data));
      invoke("write_terminal", { bytes }).catch(() => {});
    });

    let disposed = false;
    let unlisten: (() => void) | null = null;

    // Replays recent console output before registering the live listener,
    // so a freshly (re)mounted terminal — the very first one at startup, or
    // any later detach/reattach cycle (issue #385) — catches up to the
    // current session instead of starting blank.
    //
    // Sequenced carefully to avoid a reproduced bug (issue #385 UAT: typing
    // an unterminated line while docked, then detaching, hard-wrapped just
    // that line every couple of characters in the freshly shown detached
    // window — but never when typing directly into an already-detached
    // window with nothing to replay). `Terminal.write()` processes its
    // input *asynchronously* (see `@xterm/xterm`'s own doc comment on
    // `write`), so resizing while a large write (a full history replay is
    // the only write here large enough to still be mid-flight when a resize
    // could land) is still being processed corrupts wrapping for whatever
    // hadn't been parsed yet, which is reliably the *newest* tail of the
    // buffer: exactly the freshly typed, not-yet-newline-terminated line,
    // never the older already-rendered banner lines above it. So: one
    // corrective fit after a real paint (the very first fit() above can
    // measure a container whose layout hasn't settled yet, most visibly the
    // detached window's first-ever mount), then no further fit/resize at
    // all — including via the ResizeObserver above — until the history
    // write's own completion callback confirms it's fully processed.
    (async () => {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      if (disposed) return;
      fitAddon.fit();

      let history: number[] = [];
      try {
        history = await invoke<number[]>("get_terminal_history");
      } catch (err) {
        console.error("get_terminal_history failed:", err);
      }
      if (disposed) return;
      if (history.length > 0) {
        await new Promise<void>((resolve) => term.write(new Uint8Array(history), resolve));
      }
      if (disposed) return;

      // A byte emitted live in the gap between here and the listener
      // actually registering could in principle be missed; accepted as the
      // same class of narrow race already tolerated elsewhere in #385 (e.g.
      // the detach/reattach `emit_to`-retarget race).
      unlisten = await listen<number[]>("terminal-output", (event) => {
        term.write(new Uint8Array(event.payload));
      });
      if (disposed) {
        unlisten();
        return;
      }
      resizeObserver.observe(containerRef.current!);
    })();

    return () => {
      disposed = true;
      unlisten?.();
      resizeObserver.disconnect();
      containerRef.current?.removeEventListener("contextmenu", handleContextMenu);
      unregisterOverride?.();
      selectionChangeDisposable?.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
      term.dispose();
    };
    // `editMenu` (from context) is read once here, same as `resolvedTheme`
    // above it — both are effectively stable for this effect's lifetime,
    // and re-running it would tear down and rebuild the whole terminal
    // (dispose + history replay), which neither value's own updates should
    // trigger.
  }, []);

  return (
    <>
      <div ref={containerRef} className="terminal-container" />
      {sizeMenu && (
        <div ref={sizeMenuRef} className="context-menu" style={{ top: sizeMenu.y, left: sizeMenu.x }}>
          {TERMINAL_SIZE_PRESETS.map((preset) => {
            const term = termRef.current;
            const checked = term ? term.cols === preset.cols && term.rows === preset.rows : false;
            return (
              <div
                key={`${preset.cols}x${preset.rows}`}
                className="context-menu-item"
                onClick={() => {
                  closeSizeMenu();
                  resizeToGrid(preset.cols, preset.rows).catch((err) => console.error("resizeToGrid failed:", err));
                }}
              >
                <span className="context-menu-item-check">{checked && <i className="codicon codicon-check" />}</span>
                {preset.cols} x {preset.rows}
              </div>
            );
          })}
          <div className="context-menu-separator" />
          <div
            className="context-menu-item"
            onClick={() => {
              closeSizeMenu();
              setPreferencesOpen(true);
            }}
          >
            <span className="context-menu-item-check" />
            Preferences…
          </div>
        </div>
      )}
      <TerminalPreferencesDialog
        open={preferencesOpen}
        onClose={() => setPreferencesOpen(false)}
        onSaved={(preferences) => {
          applyTextPreferences(preferences.text);
          applyCursorPreferences(preferences.cursor);
          applyCompatibilityPreferences(preferences.compatibility);
        }}
        defaultForeground={(resolvedTheme === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME).foreground!}
        defaultBackground={(resolvedTheme === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME).background!}
        defaultCursorColor={(resolvedTheme === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME).cursor!}
        defaultCursorAccentColor={DEFAULT_CURSOR_ACCENT_COLOR}
      />
    </>
  );
}
