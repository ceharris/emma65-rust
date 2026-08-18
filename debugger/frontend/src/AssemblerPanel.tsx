import { useEffect, useRef } from "react";
import { EditorState, Extension } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEditMenuOverride } from "./EditMenuContext";

/**
 * Follows the app's light/dark theme via CSS custom properties (`--color-*`,
 * defined in `global.scss`) rather than `TerminalPanel.tsx`'s `useTheme()` +
 * recompute-on-change approach — CodeMirror renders through real DOM/CSS
 * (unlike xterm's canvas), so a `var(--color-*)` reference here just tracks
 * the app's theme automatically as the cascade updates, with no JS involved.
 *
 * `!important` on the four overridden properties is deliberate: CodeMirror's
 * built-in default theme only ever applies its `&light` variant of each of
 * these (gutter background/color, active line, caret color) because nothing
 * in this editor ever sets CodeMirror's own `dark` theme flag — confirmed by
 * reading `@codemirror/view`'s base theme, whose `&light`/`&dark` selectors
 * resolve to equal-specificity, mount-order-dependent rules. `!important`
 * sidesteps needing to depend on that ordering. There's no `drawSelection()`
 * extension here (not needed at this unit's scope), so selection is the
 * browser's native `::selection`, not CodeMirror's `.cm-selectionBackground`
 * layer — hence targeting `.cm-content ::selection` instead.
 */
const assemblerEditorTheme = EditorView.theme({
  "&": {
    backgroundColor: "var(--color-bg)",
    color: "var(--color-fg)",
  },
  ".cm-content": {
    caretColor: "var(--color-fg) !important",
  },
  ".cm-gutters": {
    backgroundColor: "var(--color-bg-alt) !important",
    color: "var(--color-muted) !important",
    border: "none",
    borderRight: "1px solid var(--color-border)",
  },
  ".cm-activeLineGutter, .cm-activeLine": {
    backgroundColor: "var(--color-bg-hover) !important",
  },
  ".cm-content::selection, .cm-content *::selection": {
    backgroundColor: "var(--color-bg-selected) !important",
  },
});

/**
 * The dock panel hosting the assembler source editor (issue #474 debugger
 * integration, Unit 2). Mounts a bare CodeMirror 6 `EditorView` imperatively
 * — matching `TerminalPanel.tsx`'s `useRef`+`useEffect` xterm-mount pattern
 * rather than a third-party React wrapper, since none is used anywhere else
 * in this codebase. In-memory buffer only for this unit: no `invoke` calls,
 * no assemble/load wiring (Unit 3), no file Open/Save (Unit 4).
 */
export default function AssemblerPanel() {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const editMenu = useEditMenuOverride();

  useEffect(() => {
    const copySelection = () => {
      const view = viewRef.current;
      if (!view) return;
      const text = view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to);
      if (text) writeText(text).catch((err) => console.error("copy to clipboard failed:", err));
    };
    const cutSelection = () => {
      const view = viewRef.current;
      if (!view) return;
      const { from, to } = view.state.selection.main;
      if (from === to) return;
      const text = view.state.sliceDoc(from, to);
      writeText(text)
        .then(() => view.dispatch({ changes: { from, to, insert: "" } }))
        .catch((err) => console.error("cut to clipboard failed:", err));
    };
    const pasteClipboard = () => {
      const view = viewRef.current;
      if (!view) return;
      readText()
        .then((text) => {
          if (!text) return;
          const { from, to } = view.state.selection.main;
          view.dispatch({ changes: { from, to, insert: text }, selection: { anchor: from + text.length } });
        })
        .catch((err) => console.error("paste from clipboard failed:", err));
    };

    const extensions: Extension[] = [
      lineNumbers(),
      history(),
      keymap.of([...defaultKeymap, ...historyKeymap]),
      assemblerEditorTheme,
      // Future extension point (out of scope for this unit): a
      // `StreamLanguage`-based 6502/65C02 syntax highlighter slots in here
      // as one more array entry once it exists — no rework of mount/
      // lifecycle code needed.
      EditorView.updateListener.of((update) => {
        if (update.selectionSet && editMenu) editMenu.notifyChanged();
      }),
    ];

    const view = new EditorView({
      state: EditorState.create({ doc: "", extensions }),
      parent: containerRef.current!,
    });
    viewRef.current = view;

    // Registers an Edit-menu override (see `EditMenuContext.tsx`) since
    // CodeMirror's contenteditable surface isn't recognized by the generic
    // `<input>`/`<textarea>` fallback there — without this, the native Edit
    // menu's Cut/Copy/Paste silently no-ops while focus is in this editor.
    const unregisterOverride = editMenu?.registerOverride(() => {
      if (!view.hasFocus) return null;
      const hasSelection = !view.state.selection.main.empty;
      return { canCut: hasSelection, canCopy: hasSelection, canPaste: true, cut: cutSelection, copy: copySelection, paste: pasteClipboard };
    }) ?? null;

    return () => {
      unregisterOverride?.();
      viewRef.current = null;
      view.destroy();
    };
    // `editMenu` (from context) is read once here, same as `TerminalPanel.tsx`'s
    // construction effect — re-running this on every context change would
    // tear down and rebuild the whole editor.
  }, []);

  return <div ref={containerRef} className="assembler-container" />;
}
