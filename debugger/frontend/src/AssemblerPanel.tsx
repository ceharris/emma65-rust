import { useCallback, useEffect, useRef, useState } from "react";
import { EditorState, Extension, Text } from "@codemirror/state";
import { EditorView, KeyBinding, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentLess, insertTab } from "@codemirror/commands";
import { indentUnit } from "@codemirror/language";
import { Diagnostic, lintGutter, setDiagnostics } from "@codemirror/lint";
import { invoke } from "@tauri-apps/api/core";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEditMenuOverride } from "./EditMenuContext";
import { useExecutionContext } from "./ExecutionContext";
import { usePanelHeaderAction } from "./layout/panelHeaderActions";
import "./styles/assembler.scss";

interface AssembleDiagnostic {
  line: number;
  column: number;
  message: string;
}

interface SegmentSummary {
  origin: number;
  length: number;
}

interface AssembleReport {
  success: boolean;
  diagnostics: AssembleDiagnostic[];
  segments: SegmentSummary[];
  symbol_count: number;
}

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
 * Maps a 1-based `(line, column)` position from `emma65::assembler`'s
 * scanner (`src/assembler/scanner.rs`) to a 0-based CodeMirror document
 * offset. Columns are tab-expanded there: the scanner advances the column
 * by 8 for every `\t` and by 1 for every other character (`TAB_SIZE` in
 * `scanner.rs`), not by counting characters — so this must walk the line
 * character-by-character rather than computing `line.from + (column - 1)`,
 * which is wrong on any line containing a tab before the target column.
 */
function lineColToOffset(doc: Text, line: number, column: number): number {
  const lineObj = doc.line(Math.min(Math.max(line, 1), doc.lines));
  const text = lineObj.text;
  let col = 1;
  let i = 0;
  while (i < text.length && col < column) {
    col += text.charCodeAt(i) === 9 /* '\t' */ ? 8 : 1;
    i++;
  }
  return Math.min(lineObj.from + i, lineObj.to);
}

/** Converts a backend `AssembleDiagnostic` to a CodeMirror `Diagnostic`, covering one character (a non-empty range renders better than a zero-width one). */
function toCodeMirrorDiagnostic(doc: Text, d: AssembleDiagnostic): Diagnostic {
  const from = lineColToOffset(doc, d.line, d.column);
  const to = Math.min(from + 1, doc.lineAt(from).to);
  return { from, to, severity: "error", message: d.message };
}

/**
 * The dock panel hosting the assembler source editor (issue #474 debugger
 * integration, Units 2-3). Mounts a bare CodeMirror 6 `EditorView`
 * imperatively — matching `TerminalPanel.tsx`'s `useRef`+`useEffect` xterm-
 * mount pattern rather than a third-party React wrapper, since none is used
 * anywhere else in this codebase. In-memory buffer only: no file Open/Save
 * (Unit 4).
 */
export default function AssemblerPanel() {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const editMenu = useEditMenuOverride();
  const { execState } = useExecutionContext();
  const [report, setReport] = useState<AssembleReport | null>(null);

  // On-demand only, never live-as-you-type — assembling has a real side
  // effect (writing memory via `Bus::patch`), so it must never fire
  // implicitly on a debounce timer the way pure-syntax linting normally
  // does.
  const runAssemble = useCallback(async () => {
    const view = viewRef.current;
    if (!view) return;
    const source = view.state.doc.toString();
    try {
      const result = await invoke<AssembleReport>("assemble_and_load", { source });
      setReport(result);
      const diagnostics = result.diagnostics.map((d) => toCodeMirrorDiagnostic(view.state.doc, d));
      view.dispatch(setDiagnostics(view.state, diagnostics));
    } catch (e) {
      console.error("assemble_and_load failed:", e);
    }
  }, []);

  usePanelHeaderAction("assembler", {
    title: "Assemble & Load",
    onClick: runAssemble,
    disabled: execState !== "stopped",
    disabledTitle: "Stop the CPU to assemble",
  });

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

    // Tab isn't part of `defaultKeymap` — CodeMirror leaves it out by
    // default so it keeps its usual browser role of moving focus to the
    // next control, unless a consumer opts in. This is a source editor,
    // where users expect Tab to indent instead, so opt in explicitly
    // (accepting that Tab no longer tabs out of this panel; `defaultKeymap`
    // already carries CodeMirror's own escape hatch for this, Ctrl-m /
    // Shift-Alt-m on macOS, bound to `toggleTabFocusMode`, which
    // temporarily restores Tab's native focus-moving behavior for a
    // keyboard-only user who needs to leave the editor).
    //
    // Deliberately `insertTab`, not the built-in `indentWithTab` binding
    // (which runs `indentMore`) — `indentMore` always reindents the
    // *entire current line* from its start, which is right for a selected
    // block of lines but wrong for the common case of a cursor mid-line
    // with nothing selected: a user typing a mnemonic then hitting Tab to
    // align a comment expects a tab character inserted at the cursor, not
    // the whole line's leading whitespace rewritten. `insertTab` handles
    // both: it inserts `"\t"` at the cursor when the selection is empty,
    // and falls back to `indentMore` only when a selection spans text —
    // exactly the multi-line-block-indent case `indentMore` is for.
    const tabBinding: KeyBinding = { key: "Tab", run: insertTab, shift: indentLess };

    const extensions: Extension[] = [
      lineNumbers(),
      history(),
      // Renders diagnostic markers in the gutter; the diagnostics
      // themselves are pushed in via `setDiagnostics()` from `runAssemble`
      // above, not this extension's own (unused here) `linter()` source.
      lintGutter(),
      // `indentLess` (bound to Shift-Tab above) measures/removes leading
      // whitespace in units of `indentUnit`'s column width, which defaults
      // to 2 spaces regardless of what character Tab actually inserts —
      // so, unconfigured, Shift-Tab only undid *half* of what Tab just
      // inserted (a 2-column-wide dedent against a tab char that renders 4
      // columns wide at the default `tabSize`), while Backspace deleted
      // the tab character outright and so felt like it retreated twice as
      // far. Setting the indent unit to a literal tab keeps `indentLess`
      // (and `indentMore`/block-selection Tab, via `insertTab`'s fallback)
      // consistent with `insertTab`'s own tab-character insertion, at
      // whatever `tabSize` is in effect — `getIndentUnit` computes a
      // tab-based indent unit's column width as `tabSize * 1`.
      indentUnit.of("\t"),
      keymap.of([tabBinding, ...defaultKeymap, ...historyKeymap]),
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

  return (
    <div className="assembler-panel">
      <div ref={containerRef} className="assembler-container" />
      {report && (
        report.success ? (
          <div className="assembler-summary">
            {report.segments.reduce((sum, s) => sum + s.length, 0)} bytes across {report.segments.length} segment
            {report.segments.length === 1 ? "" : "s"}, {report.symbol_count} symbol{report.symbol_count === 1 ? "" : "s"}
          </div>
        ) : (
          <div className="assembler-diagnostics">
            {report.diagnostics.map((d, i) => (
              <div className="assembler-diagnostic" key={i}>
                {d.line}:{d.column}: {d.message}
              </div>
            ))}
          </div>
        )
      )}
    </div>
  );
}
