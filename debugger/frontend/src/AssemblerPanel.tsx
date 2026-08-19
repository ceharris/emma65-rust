import { useCallback, useEffect, useRef, useState } from "react";
import { EditorState, Extension, Text } from "@codemirror/state";
import { EditorView, KeyBinding, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentLess, insertTab } from "@codemirror/commands";
import { indentUnit } from "@codemirror/language";
import { Diagnostic, lintGutter, setDiagnostics } from "@codemirror/lint";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEditMenuOverride } from "./EditMenuContext";
import { useExecutionContext } from "./ExecutionContext";
import "./styles/assembler.scss";
import "./styles/modal.scss";

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
 * `!important` on the overridden properties is deliberate: CodeMirror's
 * built-in default theme only ever applies its `&light` variant of the
 * gutter background/color, active line, and caret color, because nothing
 * in this editor ever sets CodeMirror's own `dark` theme flag — confirmed by
 * reading `@codemirror/view`'s base theme, whose `&light`/`&dark` selectors
 * resolve to equal-specificity, mount-order-dependent rules. `!important`
 * sidesteps needing to depend on that ordering; the same reasoning applies
 * to the `.cm-lint-marker-error` override below, against `@codemirror/lint`'s
 * own base theme. There's no `drawSelection()` extension here (not needed at
 * this unit's scope), so selection is the browser's native `::selection`,
 * not CodeMirror's `.cm-selectionBackground` layer — hence targeting
 * `.cm-content ::selection` instead.
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
  // `@codemirror/lint`'s default error marker is a plain filled red circle
  // (`.cm-lint-marker-error { content: url(<svg circle>) }` in its own base
  // theme) — visually identical to `DisassemblyPanel.tsx`'s breakpoint
  // indicator (the "●" character, `.disasm-gutter.breakpoint`). Swap it for
  // the same warning-triangle codicon glyph VS Code itself uses for its
  // Markers/Problems view (`markers-view-icon`, which VS Code registers
  // against `Codicon.warning` — `@vscode/codicons` ships that glyph under
  // its base name, `codicon-warning`), rather than a bespoke SVG: first
  // reset `content` back to its normal (non-replaced-element) value so the
  // marker div can render a `::before` pseudo-element instead of the
  // built-in SVG, then render the codicon's own font glyph there.
  ".cm-lint-marker-error": {
    content: "normal !important",
  },
  ".cm-lint-marker-error::before": {
    content: '"" !important', // codicon-warning
    font: "normal normal normal 1em/1 codicon !important",
    color: "var(--color-error) !important",
    display: "inline-block",
  },
});

/**
 * Converts a backend `AssembleDiagnostic` to a CodeMirror `Diagnostic`
 * covering the diagnostic's entire source line, not just its reported
 * column. Two reasons: (1) `AssembleDiagnostic.column` is tab-expanded by
 * `emma65::assembler`'s scanner (`src/assembler/scanner.rs`, `TAB_SIZE = 8`
 * per `\t`), which doesn't match this editor's own 4-column tab stops
 * (`indentUnit.of("\t")` below plus CodeMirror's default `tabSize`), so a
 * precise column→offset mapping would misplace the marker on any line with
 * a tab before the error column — not worth chasing given how little a
 * single assembly statement has going on. (2) One statement per line is
 * this grammar's norm, so underlining the whole line reads as "this
 * statement has a problem," which is what the diagnostic actually means,
 * without needing to pinpoint a sub-token.
 */
function toCodeMirrorDiagnostic(doc: Text, d: AssembleDiagnostic): Diagnostic {
  const lineObj = doc.line(Math.min(Math.max(d.line, 1), doc.lines));
  return { from: lineObj.from, to: lineObj.to, severity: "error", message: d.message };
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
  /**
   * Guards `handleOpen`/`handleSaveAs` against showing the native file
   * chooser twice for a single Assembler > Open…/Save/Save As… menu click.
   * Empirically (issue #474 debugger integration, post-Unit-4 fix): only the
   * two actions that call into `@tauri-apps/plugin-dialog`'s `open()`/
   * `save()` double-fire when the Assembler panel is already docked — New
   * and Assemble…, which never call that plugin, never do. This points at a
   * GTK/menu-event reentrancy triggered by opening a native modal dialog
   * synchronously in response to a menu-click-sourced event, not a bug in
   * this component's own listener registration — but regardless of which
   * layer redelivers the triggering event, a synchronous ref set before the
   * first `await` reliably blocks a second dialog from ever appearing.
   */
  const nativeDialogInFlightRef = useRef(false);
  const editMenu = useEditMenuOverride();
  const { execState } = useExecutionContext();
  const [report, setReport] = useState<AssembleReport | null>(null);
  /** Absolute path of the file currently backing the editor; null for an untitled buffer. */
  const [currentPath, setCurrentPath] = useState<string | null>(null);
  /** True once the buffer has been edited since it was last loaded/saved. */
  const [isDirty, setIsDirty] = useState(false);
  /** True while the "discard unsaved changes?" confirmation is open (New only, per explicit scope). */
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  /** Error message from a failed Open/Save/Save As…; null when no error dialog is open. */
  const [fileErrorDialog, setFileErrorDialog] = useState<string | null>(null);

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

  // Keeps the native Assembler menu's Assemble… item in lockstep with the
  // CPU-must-be-stopped condition Assemble & Load always required — the
  // panel's own header button that used to carry this `disabled` state was
  // removed in favor of the native menu (mirroring the Memory panel's
  // issue #411 precedent), so this is now the only place that condition is
  // enforced client-side. New/Open…/Save/Save As… aren't CPU-gated, so only
  // this one item needs syncing (mirrors `set_memory_menu_enabled`'s call
  // site in `MemoryPanel.tsx`).
  const lastAssemblerMenuEnabledRef = useRef<boolean | null>(null);
  useEffect(() => {
    const enabled = execState === "stopped";
    if (enabled === lastAssemblerMenuEnabledRef.current) return;
    lastAssemblerMenuEnabledRef.current = enabled;
    invoke("set_assembler_menu_enabled", { enabled }).catch((e) =>
      console.error("set_assembler_menu_enabled failed:", e),
    );
  }, [execState]);

  /**
   * Replaces the whole buffer, resetting dirty/path/report state and
   * clearing any lint gutter markers left over from assembling the previous
   * buffer's contents (their positions no longer mean anything once the
   * document they were computed against is gone).
   */
  const loadDocument = useCallback((text: string, path: string | null) => {
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: text },
      selection: { anchor: 0 },
    });
    view.dispatch(setDiagnostics(view.state, []));
    setCurrentPath(path);
    setIsDirty(false);
    setReport(null);
  }, []);

  /** Clears the buffer to a new untitled document, confirming first if there are unsaved changes. */
  const handleNew = useCallback(() => {
    if (isDirty) {
      setConfirmDiscard(true);
      return;
    }
    loadDocument("", null);
  }, [isDirty, loadDocument]);

  const confirmNew = useCallback(() => {
    setConfirmDiscard(false);
    loadDocument("", null);
  }, [loadDocument]);

  /** Opens the native file chooser, reads the selected file, and loads it into the buffer. */
  const handleOpen = useCallback(async () => {
    if (nativeDialogInFlightRef.current) return;
    nativeDialogInFlightRef.current = true;
    try {
      const defaultPath = await invoke<string | null>("get_last_file_dialog_dir");
      const selected = await openFileDialog({
        multiple: false,
        filters: [{ name: "Assembly Source", extensions: ["s", "asm", "a65"] }],
        defaultPath: defaultPath ?? undefined,
      });
      if (typeof selected !== "string") return;
      try {
        const contents = await invoke<string>("read_source_file", { path: selected });
        loadDocument(contents, selected);
        invoke("set_last_file_dialog_dir", { path: selected }).catch(() => {});
      } catch (e) {
        setFileErrorDialog(String(e));
      }
    } finally {
      nativeDialogInFlightRef.current = false;
    }
  }, [loadDocument]);

  /** Opens the native save-file chooser and writes the buffer to the chosen path. */
  const handleSaveAs = useCallback(async () => {
    const view = viewRef.current;
    if (!view) return;
    if (nativeDialogInFlightRef.current) return;
    nativeDialogInFlightRef.current = true;
    try {
      const defaultPath = currentPath ?? (await invoke<string | null>("get_last_file_dialog_dir")) ?? undefined;
      const selected = await saveFileDialog({
        filters: [{ name: "Assembly Source", extensions: ["s", "asm", "a65"] }],
        defaultPath,
      });
      if (typeof selected !== "string") return;
      try {
        await invoke("write_source_file", { path: selected, contents: view.state.doc.toString() });
        setCurrentPath(selected);
        setIsDirty(false);
        invoke("set_last_file_dialog_dir", { path: selected }).catch(() => {});
      } catch (e) {
        setFileErrorDialog(String(e));
      }
    } finally {
      nativeDialogInFlightRef.current = false;
    }
  }, [currentPath]);

  /** Writes the buffer to its current path; falls back to Save As… when the buffer has none yet. */
  const handleSave = useCallback(async () => {
    const view = viewRef.current;
    if (!view) return;
    if (!currentPath) {
      await handleSaveAs();
      return;
    }
    try {
      await invoke("write_source_file", { path: currentPath, contents: view.state.doc.toString() });
      setIsDirty(false);
    } catch (e) {
      setFileErrorDialog(String(e));
    }
  }, [currentPath, handleSaveAs]);

  // The Assembler panel's file operations are also reachable via the native
  // Assembler menu (issue #474, debugger integration Unit 4) — a menu click
  // or `Alt+N`/`Alt+O`/`Alt+S`/`F9` accelerator reaches this handler as an
  // event targeted at the main window, the same way `memory-menu-action`
  // reaches `MemoryPanel.tsx`.
  useEffect(() => {
    const unlistenPromise = listen<string>("assembler-menu-action", (event) => {
      switch (event.payload) {
        case "new-assembler":
          handleNew();
          break;
        case "open-assembler":
          handleOpen();
          break;
        case "save-assembler":
          handleSave();
          break;
        case "save-as-assembler":
          handleSaveAs();
          break;
        case "assemble-load":
          runAssemble();
          break;
      }
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, [handleNew, handleOpen, handleSave, handleSaveAs, runAssemble]);

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
        // `loadDocument` (New/Open) issues its own full-doc replacement
        // transaction, which also flows through here as `docChanged` — it
        // calls `setIsDirty(false)` itself right after dispatching, and
        // since both calls land in the same React batch, that later call
        // wins over this one.
        if (update.docChanged) setIsDirty(true);
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

  /** Dismiss the discard-changes confirmation on Escape while it is open. */
  useEffect(() => {
    if (!confirmDiscard) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setConfirmDiscard(false);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [confirmDiscard]);

  /** Dismiss the file-error dialog on Escape while it is open. */
  useEffect(() => {
    if (!fileErrorDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setFileErrorDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [fileErrorDialog]);

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
                Line {d.line}: {d.message}
              </div>
            ))}
          </div>
        )
      )}

      {confirmDiscard && (
        <div className="modal-backdrop" onClick={() => setConfirmDiscard(false)}>
          <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="modal-title">Discard Changes</div>
            <div className="modal-message">
              This buffer has unsaved changes. Starting a new file will discard them. Continue?
            </div>
            <div className="modal-buttons">
              <button className="modal-btn-action modal-btn-cancel" onClick={() => setConfirmDiscard(false)}>
                Cancel
              </button>
              <button className="modal-btn-action modal-btn-ok" onClick={confirmNew}>
                Discard
              </button>
            </div>
          </div>
        </div>
      )}

      {fileErrorDialog !== null && (
        <div className="modal-backdrop" onClick={() => setFileErrorDialog(null)}>
          <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="modal-title">File Error</div>
            <div className="modal-message">{fileErrorDialog}</div>
            <div className="modal-buttons">
              <button className="modal-btn-action modal-btn-ok" onClick={() => setFileErrorDialog(null)}>
                Dismiss
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
