# Terminal Sizing & Size-Preset Menu (Issue #462)

## Context

Programs written for a fixed-size VT220-style terminal (commonly 80x24, 80x25,
132x24, etc.) often discover the screen size at runtime by positioning the
cursor absurdly far off-screen and reading back where the terminal actually
clamped it. That trick only works if the emulated terminal's row/column count
is accurate. Issue #462 reports two related problems with the debugger's
built-in terminal (`TerminalPanel.tsx`, shared by the docked panel and the
detached window):

1. There is no UI affordance to size the terminal to a specific row/column
   count — the user is reduced to trial-and-error dragging of window/panel
   borders.
2. Docking/undocking does not reliably produce a correctly-sized grid: it
   works when detached, but a docked terminal can retain stale dimensions
   from a prior detached size rather than adapting to the panel's actual
   available space.

The issue's author also left historical notes (mirrored in
`doc/terminal-sizing-plan.md`, June 2026) about display-scaling and
scrollbar-gutter miscalculation that made earlier sizing attempts brittle
under Wayland/GTK. Two things have changed since that doc was written and
should be re-verified rather than assumed still broken:

- `debugger/src-tauri/src/main.rs` now unconditionally forces
  `GDK_BACKEND=x11` on Linux (added for an unrelated titlebar-repaint bug),
  which may have already resolved the Wayland-specific scale-factor
  flakiness the old doc struggled with.
- The installed `@xterm/addon-fit` (0.11.0) already subtracts a 14px
  scrollbar gutter from its measurement whenever `scrollback !== 0`
  (`proposeDimensions()` in `addon-fit.js`), which is the exact gutter bug
  the old doc called out. That specific complaint may already be moot.

So this plan treats "make docked/detached sizing precise" as needing
verification-driven fixes rather than a full rewrite, and treats "give the
user a way to pick a size" as new, well-scoped feature work.

**Decisions made with the user, not to be revisited during implementation:**

- Selecting a preset size while **docked** resizes the terminal's dock panel
  itself (via dockview's `panel.api.setSize()`, same mechanism already used
  in `DockLayout.tsx` for Memory/Registers/Breakpoints/Stack default sizing).
  If the surrounding window is too small to fit the request, the panel gets
  whatever space is available and the terminal snaps down to the largest
  size that fits — no forced window growth.
- The size menu is a **native right-click/Ctrl-click context menu** inside
  the terminal area, identical in both docked and detached hosts (built via
  `@tauri-apps/api/menu`, already a dependency — no new Rust IPC needed for
  the menu itself). The docked panel **additionally** gets a hamburger icon
  in the dock tab header, next to the existing Detach button
  (`panelHeaderActions.tsx`), that opens the same menu — for discoverability
  without requiring the user to know to right-click. The detached window
  keeps its ordinary native title bar; no custom titlebar/decorations work.
- Font name/size **picker UI** (issue requirement #6) is out of scope for
  this plan — deferred to a future general Preferences panel (which doesn't
  exist yet anywhere in the debugger). However, the font name and size
  **are** persisted as ordinary fields in `UiConfig` (`preferences.rs`,
  same file as `terminal_scrollback`) in this unit of work, so a user can
  hand-edit `ui.toml` today to change the terminal font ahead of any UI for
  it. The sizing math should key off whatever font is actually configured,
  not hardcode assumptions that would need to change later.
- Display scale factor: auto-discovery via the browser's own scale reporting
  is the primary mechanism; a CLI override flag ships in the same unit of
  work as a fallback escape hatch, following the existing `--profile` /
  `CliArgs` pattern in `debugger/src-tauri/src/profile.rs`.

## Work Units

### 1. Font preference storage + validation (no picker UI)

- Add `terminal_font_family: Option<String>` and `terminal_font_size: Option<u32>`
  to `UiConfig` in `debugger/src-tauri/src/preferences.rs`, following the
  existing `terminal_scrollback` field's pattern (`#[serde(default)]`,
  round-trip test in the `#[cfg(test)]` module below it). `None`/absent in
  `ui.toml` means "use the platform default" — do not invent a hardcoded
  default font name the way `default_terminal_scrollback()` does for a
  number; the fallback is resolved on the frontend (below), since only the
  webview knows what fonts are actually available on the host.
- Expose a `get_terminal_font()` command (mirrors `get_terminal_scrollback`)
  returning both fields.
- In `TerminalPanel.tsx`, replace the current hardcoded `fontSize: 14` and
  bare `--font-mono` CSS var lookup (lines ~66-76) with: fetch the preference
  once (same effect pattern as the existing scrollback fetch at line ~58),
  then validate — if `terminal_font_family` is unset, or if a canvas-based
  glyph-width probe shows it isn't actually monospace (render `"i"` and
  `"M"`/`"W"` at the candidate font and compare advance widths; unequal
  means non-monospace), fall back to the platform default monospace font
  (the existing `--font-mono` CSS var) at 14px, matching the terminal's
  pre-#462 hardcoded size so leaving the preference unconfigured is a no-op
  for existing users. Otherwise use the configured family/size.
- This validation helper is also what Work Unit 2's cell-metrics code needs
  to trust the font it's measuring, so implement it in the shared
  `terminalSizing.ts` module (Work Unit 2) rather than inline in the panel.

### 2. Shared terminal-sizing utility (frontend)

New module, e.g. `debugger/frontend/src/terminalSizing.ts`, extracted out of
`TerminalPanel.tsx` so both the resize-on-container-change path and the
size-menu's resize-to-preset path share one source of truth for cell metrics.

- `measureCell(term)` — reads `term._core._renderService.dimensions.css.cell`
  (same private path `addon-fit` already relies on) for the actual rendered
  glyph width/height in CSS px, plus the scrollbar-gutter constant
  (`overviewRuler?.width || 14`, matching `addon-fit`'s own logic) when
  scrollback is enabled.
- `pixelSizeForGrid(term, cols, rows)` — inverse of `FitAddon.proposeDimensions()`:
  given a target grid, returns the CSS-px container size (including gutter
  and the container's own padding) needed to display it without clipping.
  This is the function both the docked (`setSize`) and detached
  (`Window.setSize`) resize paths call.
- `logicalSizeForCssPixels(cssWidth, cssHeight)` — converts a CSS-px size to
  a Tauri `LogicalSize` for the detached window using
  `getCurrentWindow().scaleFactor()` (async) or the CLI override (below) in
  place of it. This conversion is **only** needed for the detached OS-window
  path; the docked path stays entirely in CSS/logical-px within the same
  webview and needs no scale conversion (confirmed by reading `addon-fit`'s
  source — it works purely in `getComputedStyle` CSS px).

CLI override plumbing:
- Extend `CliArgs` in `debugger/src-tauri/src/profile.rs` with an optional
  `--scale-factor <f64>` flag, following the existing `--profile` /
  `--restore-layout` fields.
- Store it in a small managed state (mirrors `TerminalHistory`'s pattern in
  `terminal.rs`) and expose a `get_terminal_scale_factor_override()` command
  returning `Option<f64>`, read once by `terminalSizing.ts` alongside the
  existing `get_terminal_scrollback` fetch in `TerminalPanel.tsx`.

### 3. Correct snap-to-grid behavior on resize/attach/detach

Audit and fix the existing resize triggers in `TerminalPanel.tsx` so each one
recomputes via the Work Unit 2 utility instead of a bare `fitAddon.fit()`
where that's been shown to be insufficient:

- The `ResizeObserver` callback (dock split-drag, tab activation) — verify
  `addon-fit`'s own gutter handling is sufficient here (it operates in CSS px
  within the same webview, so likely already correct); only replace if
  testing turns up real clipping.
- Detached-window native resize — currently has no explicit resize handler
  at all (relies on the `ResizeObserver` over the container, which does fire
  on OS window resize too, so likely already covered — verify rather than
  assume a gap).
- Detach/reattach remount — `TerminalPanel.tsx` already fully remounts on
  every detach/reattach (per its `useEffect` cleanup), so this should already
  pick up the current container's real size on mount via the existing
  post-paint `fitAddon.fit()` call (line ~189). Verify the "docked terminal
  keeps stale detached dimensions" bug from the issue is actually still
  reproducible on current `main` before writing a fix for it — it may
  already have been resolved by the #385 remount redesign, in which case
  this work unit shrinks to "confirm and add a regression test/note," not a
  new fix.

**Outcome:** all three paths already resolve correctly on current `main`, no
code fix needed — confirmed both by code audit and by live interactive
testing (dock-split drag, detached-window native resize, and a
detach/reattach cycle, all against the running `snake` profile) once GUI
automation tooling (`xdotool`/`imagemagick`) became available; see that
unit's PR for the full verification notes, including one incidental,
non-app finding about this environment's window manager not honoring
low-level `XResizeWindow` requests (a properly-negotiated EWMH resize —
what a real user drag or Tauri's own `window.set_size()` both produce —
worked cleanly).

- The `ResizeObserver` callback: `addon-fit`'s `proposeDimensions()` reads
  the container's actual `getComputedStyle` box in CSS px, entirely inside
  the webview's own DOM — there's no Tauri logical/physical conversion in
  this path to get wrong, so it's correct regardless of display scale.
- Detached-window native resize: `.terminal-container` is `width: 100%;
  height: 100%` (`global.scss`) all the way up through `html`/`body`/`#root`
  in `terminal-detached.html`'s document, which *is* the detached window's
  viewport — an OS-level resize changes that element's border box directly,
  so the existing `ResizeObserver` already fires on it with no separate
  handler needed.
- Detach/reattach remount: `DockLayout.tsx`'s `closeTerminalPanel`/
  `terminal-reattached` handler always closes the dock panel and later
  `addPanel`s a brand new one (never reparents), so every detach/reattach is
  a full unmount + remount of `TerminalPanel` — a fresh `Terminal` fit
  against whichever host it lands in next, with no code path that could
  carry a size measured against the previous host forward. The "docked
  terminal keeps stale detached dimensions" bug from the original report
  is architecturally impossible in the current close/`addPanel` + full
  remount design (confirms the #385 remount redesign already fixed it).

Documented inline as code comments in `TerminalPanel.tsx` (near the
`ResizeObserver` declaration and the component's top doc comment) rather
than as a standalone regression test, since the frontend has no test runner
configured yet.

### 4. Size-preset context menu + docked header icon

- In `TerminalPanel.tsx`, register a `contextmenu` handler on the terminal
  container that builds and pops up a `@tauri-apps/api/menu` `Menu` with the
  four fixed sizes from the issue (80x24, 132x24, 80x43, 132x43) plus a
  checkmark/indicator on whichever preset the current grid matches, if any.
  Selecting an item calls Work Unit 2's `pixelSizeForGrid` and then either:
  - **Docked:** the dockview panel API's `setSize()` (needs a way for
    `TerminalPanel.tsx` to reach the panel API — check whether `DockLayout.tsx`
    already threads panel refs down via context, or whether this needs a
    small addition there, e.g. reusing the `usePanelHeaderAction`
    provider's existing panel-id plumbing).
  - **Detached:** `getCurrentWindow().setSize(new LogicalSize(...))` using
    Work Unit 2's scale conversion.
- Add a hamburger icon action for the docked case via the existing
  `usePanelHeaderAction` mechanism (`panelHeaderActions.tsx`), same pattern
  as the current Detach button, opening the identical menu built above
  (factor the menu-building into a shared function so both triggers use it).
- No change needed to the detached window's stripped native app menu — the
  context menu is independent of it.

### Out of scope (explicitly deferred)

- Font name/size **picker UI** (issue requirement #6). Storage/validation of
  the preference is in scope (Work Unit 1) — only the UI for editing it is
  deferred, to a future general Preferences panel that doesn't exist yet.

## Key files

- `debugger/frontend/src/TerminalPanel.tsx` — font preference fetch/apply,
  resize triggers, new context menu wiring, header-icon registration
- `debugger/frontend/src/terminalSizing.ts` (new) — shared metrics/sizing
  math, monospace-validation helper
- `debugger/frontend/src/layout/panelHeaderActions.tsx`,
  `debugger/frontend/src/layout/DockLayout.tsx` — hamburger header icon,
  panel API access for docked `setSize()`
- `debugger/src-tauri/src/preferences.rs` — `terminal_font_family` /
  `terminal_font_size` fields, `get_terminal_font()` command
- `debugger/src-tauri/src/profile.rs` — `--scale-factor` CLI flag
- `debugger/src-tauri/src/terminal.rs` (or a small new module) — scale-factor
  override managed state + command

## Verification

- `cargo build --workspace` and `cargo clippy` clean.
- Run the debugger (`cargo tauri dev` from `debugger/src-tauri`) against the
  default TaliForth profile.
- Docked: right-click the terminal, confirm the menu appears and each of the
  four presets resizes the dock panel to exactly that grid (no clipped last
  row/column); confirm the header hamburger icon opens the same menu.
- Detach the terminal, repeat the same size-menu checks against the native
  OS window; drag-resize the window and confirm the grid snaps to whole
  cells with no clipping.
- Dock/undock a few times and confirm the terminal doesn't retain a stale
  grid size from before the transition (the original bug report).
- From within the emulated program (or a quick test via `printf`/manual ACIA
  writes), send `ESC[999;999H` followed by a cursor-position-report query and
  confirm the reported position matches the actual configured grid, for both
  a docked and a detached size.
- If practical, test under at least one non-100% GNOME text scaling factor
  (e.g. 125%) to confirm the detached-window logical/CSS pixel conversion
  holds; otherwise note in the PR that this couldn't be verified locally.
- Test `--scale-factor` override actually changes the detached window's
  resulting size at a given preset.

## Workflow

This plan is implemented as four sequential units of work (numbered above).
For each: create a branch named for the unit, do the work plus the
validation described above, commit, push to origin, and open a PR calling
out any manual UAT needed. Await explicit instruction before merging that PR
or starting the next unit.
