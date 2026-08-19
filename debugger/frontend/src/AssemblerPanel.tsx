import { useCallback, useEffect, useRef, useState } from "react";
import { EditorState, Extension, Text } from "@codemirror/state";
import { EditorView, KeyBinding, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentLess, insertTab } from "@codemirror/commands";
import { indentUnit } from "@codemirror/language";
import { Diagnostic, forEachDiagnostic, lintGutter, setDiagnostics } from "@codemirror/lint";
import { invoke } from "@tauri-apps/api/core";
import { DockviewPanelApi } from "dockview-react";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEditMenuOverride } from "./EditMenuContext";
import { useExecutionContext } from "./ExecutionContext";
import { registerAssemblerPanel } from "./layout/assemblerMenuActions";
import "./styles/assembler.scss";
import "./styles/modal.scss";

/** Tab title shown when no file is open — kept in sync with `PANEL_TITLES.assembler` in `panelRegistry.tsx`. */
const BASE_TITLE = "Assembler";

/** Basename of a file path, for the tab title — same pattern as `TracePanel.tsx`'s `basename`. */
function basename(path: string): string {
  return path.split(/[/\\]/).pop() ?? path;
}

/** 4-digit uppercase hex, no `$`/`0x` prefix — matches `MemoryPanel.tsx`'s `fmtAddr`/`DisassemblyPanel.tsx`'s `formatAddr`. */
function fmtAddr(addr: number): string {
  return addr.toString(16).toUpperCase().padStart(4, "0");
}

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
  // The lint-hover popover (`.cm-tooltip`, from `@codemirror/view`'s own
  // base theme) is stuck on its `&light` variant for the same reason as the
  // gutter marker above — `&light .cm-tooltip` hardcodes a light-gray
  // background (`#f5f5f5`) with no explicit text color, so in dark mode the
  // popover renders this app's light `--color-fg` text against that
  // near-white background: unreadable. The popover's container div is
  // appended to `document.body` (outside `.cm-editor`) but still carries
  // this theme's generated class (`view.themeClasses`), so a plain
  // `EditorView.theme()` selector still reaches it the same way it reaches
  // in-editor elements like the gutter.
  ".cm-tooltip": {
    backgroundColor: "var(--color-bg-alt) !important",
    color: "var(--color-fg) !important",
    border: "1px solid var(--color-border) !important",
  },
});

/**
 * A CodeMirror `Diagnostic` carrying the source line number of the
 * `AssembleDiagnostic` it was built from, so a diagnostic cleared from the
 * editor (see the update listener below) can also be matched back to and
 * removed from the plain-text diagnostic list rendered below the editor.
 * `@codemirror/lint` stores whatever object it's given verbatim in its
 * decoration spec (`LintState.init`, in the package's own source) — it
 * doesn't clone or strip extra properties — so this survives untouched
 * through `setDiagnostics()`/`forEachDiagnostic()`.
 */
interface EditorDiagnostic extends Diagnostic {
  reportLine: number;
}

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
function toCodeMirrorDiagnostic(doc: Text, d: AssembleDiagnostic): EditorDiagnostic {
  const lineObj = doc.line(Math.min(Math.max(d.line, 1), doc.lines));
  return { from: lineObj.from, to: lineObj.to, severity: "error", message: d.message, reportLine: d.line };
}

interface AssemblerPanelProps {
  /**
   * The dockview panel API, threaded down from `panelRegistry.tsx` (same
   * pattern as `TerminalPanel.tsx`'s `dockPanelApi`) so the dock tab's title
   * can be kept in sync with the currently open file.
   */
  dockPanelApi?: DockviewPanelApi;
}

/**
 * The dock panel hosting the assembler source editor (issue #474 debugger
 * integration, Units 2-3). Mounts a bare CodeMirror 6 `EditorView`
 * imperatively — matching `TerminalPanel.tsx`'s `useRef`+`useEffect` xterm-
 * mount pattern rather than a third-party React wrapper, since none is used
 * anywhere else in this codebase. In-memory buffer only: no file Open/Save
 * (Unit 4).
 */
export default function AssemblerPanel({ dockPanelApi }: AssemblerPanelProps = {}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  /**
   * Guards `handleOpen`/`handleSaveAs` against showing the native file
   * chooser twice for a single Assembler > Open…/Save/Save As… menu click.
   * The actual root cause of the double-dialog/lost-event bug (issue #474
   * debugger integration, Unit 4 follow-up) turned out to be a mount-timing
   * race, fixed by routing `assembler-menu-action` through
   * `assemblerMenuActions.ts` instead — see that module's doc comment. This
   * guard is kept as a cheap, harmless defense against any other path that
   * might invoke these two handlers concurrently (e.g. a leftover dockview
   * panel instance from before that fix), not as the primary fix.
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
  /**
   * A successful `assemble_preview` result awaiting the user's explicit
   * confirmation before anything is written to memory; null when the
   * confirmation dialog is closed. `source` is the exact buffer text the
   * preview was computed from — `confirmAssemble` re-sends this same string
   * to `assemble_and_load` rather than re-reading the (possibly since-edited)
   * live buffer, so what the dialog describes is exactly what gets written.
   */
  const [pendingAssemble, setPendingAssemble] = useState<{ source: string; segments: SegmentSummary[]; symbolCount: number } | null>(
    null,
  );

  // Keeps the dock tab's title in sync with the currently open file, so the
  // path is visible at a glance without needing to hover/inspect anything —
  // just the filename (not the full directory), with a trailing "*" while
  // there are unsaved changes, matching common editor-tab conventions.
  useEffect(() => {
    if (!dockPanelApi) return;
    const name = currentPath ? basename(currentPath) : null;
    const dirtyMark = isDirty ? "*" : "";
    dockPanelApi.setTitle(name ? `${BASE_TITLE} — ${name}${dirtyMark}` : `${BASE_TITLE}${dirtyMark}`);
  }, [dockPanelApi, currentPath, isDirty]);

  // On-demand only, never live-as-you-type — assembling has a real eventual
  // side effect (writing memory via `Bus::patch`), so it must never fire
  // implicitly on a debounce timer the way pure-syntax linting normally does.
  // This step itself only *previews* the result (`assemble_preview` touches
  // no memory) — a successful assembly opens the confirmation dialog below
  // rather than writing immediately; a failed one applies diagnostics exactly
  // as before and skips the dialog entirely, since there is nothing to
  // confirm.
  const runAssemble = useCallback(async () => {
    const view = viewRef.current;
    if (!view) return;
    const source = view.state.doc.toString();
    try {
      const result = await invoke<AssembleReport>("assemble_preview", { source });
      if (!result.success) {
        setReport(result);
        const diagnostics = result.diagnostics.map((d) => toCodeMirrorDiagnostic(view.state.doc, d));
        view.dispatch(setDiagnostics(view.state, diagnostics));
        return;
      }
      setPendingAssemble({ source, segments: result.segments, symbolCount: result.symbol_count });
    } catch (e) {
      console.error("assemble_preview failed:", e);
    }
  }, []);

  const cancelAssemble = useCallback(() => setPendingAssemble(null), []);

  /** Writes the previewed assembly to memory — the only path that actually calls `assemble_and_load`. */
  const confirmAssemble = useCallback(async () => {
    const pending = pendingAssemble;
    setPendingAssemble(null);
    const view = viewRef.current;
    if (!pending || !view) return;
    try {
      const result = await invoke<AssembleReport>("assemble_and_load", { source: pending.source });
      setReport(result);
      const diagnostics = result.diagnostics.map((d) => toCodeMirrorDiagnostic(view.state.doc, d));
      view.dispatch(setDiagnostics(view.state, diagnostics));
    } catch (e) {
      console.error("assemble_and_load failed:", e);
    }
  }, [pendingAssemble]);

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
  // or `Alt+N`/`Alt+O`/`Alt+S`/`F9` accelerator reaches this handler through
  // `assemblerMenuActions.ts`'s `registerAssemblerPanel`, not a direct
  // `listen("assembler-menu-action", ...)` call here: this panel's dock tab
  // is created lazily on first reveal, so a `listen()` registered from this
  // component's own mount effect can lose the race against the very click
  // that's revealing it (see that module's doc comment for the full
  // explanation, including how this was confirmed with diagnostic logging).
  // `registerAssemblerPanel` is backed by an always-active listener mounted
  // once in `DockLayout.tsx`, and replays any action that arrived before
  // this panel was ready.
  useEffect(() => {
    return registerAssemblerPanel((action) => {
      switch (action) {
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
        if (update.docChanged) {
          setIsDirty(true);
          // `@codemirror/lint`'s diagnostics state field just maps marker
          // positions through each change (`lintState.update`, in the
          // package's own source) — it never drops a marker on its own,
          // since we push diagnostics manually via `setDiagnostics()`
          // rather than using the `linter()` extension's automatic
          // re-lint-on-change. We don't re-assemble as the user types
          // (assembling has a real bus-write side effect), but a marker
          // anchored to a line whose text has since changed is actively
          // misleading, so drop just the diagnostics whose range the edit
          // touched, leaving markers on untouched lines alone.
          const survivors: EditorDiagnostic[] = [];
          const clearedLines = new Set<number>();
          forEachDiagnostic(update.startState, (diagnostic, from, to) => {
            const { reportLine } = diagnostic as EditorDiagnostic;
            if (update.changes.touchesRange(from, to)) {
              clearedLines.add(reportLine);
              return;
            }
            survivors.push({ ...(diagnostic as EditorDiagnostic), from: update.changes.mapPos(from), to: update.changes.mapPos(to) });
          });
          if (clearedLines.size > 0) {
            update.view.dispatch(setDiagnostics(update.state, survivors));
            // Keep the plain-text diagnostic list below the editor (driven
            // by `report`, not CodeMirror's own diagnostics state) in sync
            // with the markers just cleared above — otherwise a message
            // stays listed there for a line whose in-editor marker already
            // disappeared, which reads as the fix not having taken effect.
            setReport((prev) => {
              if (!prev) return prev;
              const diagnostics = prev.diagnostics.filter((d) => !clearedLines.has(d.line));
              if (diagnostics.length === prev.diagnostics.length) return prev;
              return diagnostics.length === 0 ? null : { ...prev, diagnostics };
            });
          }
        }
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

  /** Dismiss the Assemble confirmation (without writing to memory) on Escape while it is open. */
  useEffect(() => {
    if (!pendingAssemble) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPendingAssemble(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [pendingAssemble]);

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

      {pendingAssemble && (
        <div className="modal-backdrop" onClick={cancelAssemble}>
          <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="modal-title">Confirm Assemble</div>
            <div className="modal-message">
              This will write the following segment{pendingAssemble.segments.length === 1 ? "" : "s"} to memory,
              overwriting any existing contents:
            </div>
            <ul className="assembler-confirm-segments">
              {pendingAssemble.segments.map((s, i) => (
                <li key={i}>
                  {fmtAddr(s.origin)}: {s.length} byte{s.length === 1 ? "" : "s"}
                </li>
              ))}
            </ul>
            <div className="modal-buttons">
              <button className="modal-btn-action modal-btn-cancel" onClick={cancelAssemble}>
                Cancel
              </button>
              <button className="modal-btn-action modal-btn-ok" onClick={confirmAssemble}>
                Write to Memory
              </button>
            </div>
          </div>
        </div>
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
