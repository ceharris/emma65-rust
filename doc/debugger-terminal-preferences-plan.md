# Terminal Preferences Dialog (Issue #467)

## Context

Issue #462 gave the debugger's built-in terminal a size-preset menu but explicitly
deferred a font-picker UI, and today the terminal's colors, cursor shape/blink, and
Backspace/Delete key behavior are all hardcoded in `TerminalPanel.tsx` with no user
control at all. Issue #467 asks for a "Preferences…" item on that same menu that opens
a tabbed modal (Text / Cursor / Compatibility) covering: font family/size, blink,
foreground/background + 16-color ANSI palette, and scrollback length (Text); active/
inactive cursor shape, blink, and color/accent color (Cursor); and Backspace/Delete key
behavior — ASCII BS, ASCII DEL, or the ANSI DCH escape sequence (Compatibility). The
issue also asks that preferences be modeled as a structured/nested block (not flat
fields) with built-in defaults, in a way that anticipates future per-profile overrides.

Two research passes (backend `preferences.rs`/`UiConfig` persistence pattern, and the
frontend `TerminalPanel.tsx`/dialog/menu code) plus direct verification against the
installed `@xterm/xterm` package establish the concrete constraints this plan works
within — captured below as decisions rather than re-derived during implementation.

**Decisions made with the user, not to be revisited during implementation:**

- **Blink:** staying on the currently-installed stable `@xterm/xterm` 6.0.0 (not the
  6.1.0 beta). Verified directly against both the stable and beta typings: 6.0.0
  exposes only a boolean `cursorBlink` (on/off, fixed non-configurable interval) and no
  public API at all to control the text blink attribute (SGR 5). Scope: **Cursor tab**
  keeps a real "Blinking cursor" enable checkbox (maps to xterm's `cursorBlink`), no
  rate input. **Text tab drops "Blinking text" (both enable and rate) entirely** — not
  controllable via 6.0.0's public API. No rate input anywhere in this plan.
- **Font face:** plain free-text input, no font-chooser control. Verified WebKitGTK
  (this app's Linux webview) doesn't support the Local Font Access API needed to
  enumerate installed system fonts, so the issue's "if feasible" hedge resolves to "not
  feasible." Reuses the monospace-detection probe already built for #462 Work Unit 1
  (`terminalSizing.ts`'s `isMonospaceFont`), but the *dialog's* validation behavior is
  stricter than that unit's original silent-fallback behavior: if the entered font name
  is unknown or not monospace, the dialog shows an inline error and **blocks Save**
  (Enter/Save button disabled or a rejected commit) rather than silently substituting
  the platform default. The existing silent-fallback logic in `resolveTerminalFont`
  remains as-is for *applying* an already-stored value at terminal-construction time
  (defensive handling for a hand-edited or otherwise corrupted `ui.toml`) — only the
  dialog's own save path becomes strict.
- **Color chooser:** a small custom in-app popover (16 ANSI-labeled swatches for
  one-click pick) plus a "Custom…" swatch that opens the OS-native `<input
  type="color">` for arbitrary 24-bit RGB. No color-picker component exists in this
  codebase today (confirmed by search) — this is new frontend code either way; this
  option matches the issue's literal wording ("standard 16 colors... or choosing any
  24-bit RGB color") more closely than relying on the OS picker alone.
- **Data model:** a new nested `TerminalPreferences` struct (with `text`/`cursor`/
  `compatibility` sub-structs) replaces today's flat `terminal_font_family`/
  `terminal_font_size`/`terminal_scrollback` fields on `UiConfig`, folding those three
  existing values into the new `text` sub-struct. This is a one-time reset of those
  three fields for any existing local `ui.toml` (old keys are simply no longer read) —
  acceptable since `ui.toml` is single-user local state, and matches what the issue
  explicitly asks for ("organized as a structure... with appropriate substructures").
  `terminal_preferences` is its own top-level field on `UiConfig` (not flattened) so it
  can be lifted into a future profile-level override layer later without a reshape —
  no override/merge mechanism exists yet anywhere in the debugger crate, so none is
  built now; this is just keeping the field boundary clean for that future work.

## Work Units

### 1. Backend data model (no UI)

In `debugger/src-tauri/src/preferences.rs`, following the existing `UiConfig`
field/test conventions (`#[serde(default)]`, hand-written `impl Default`, doc comment
citing #467, `defaults_*_when_missing`/`round_trips_*` tests):

- Add nested structs (exact field lists per the issue's Text/Cursor/Compatibility
  lists above):
  - `TerminalTextPreferences` — `font_family: Option<String>`, `font_size:
    Option<u32>` (migrated from the existing top-level fields), `scrollback: u32`
    (migrated from `terminal_scrollback`, keep its `default_terminal_scrollback()`
    fn), `foreground`/`background: Option<String>` (hex), and a 16-entry ANSI palette
    — represent it as named fields mirroring xterm's `ITheme` (`black`, `red`,
    `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `bright_black`, …,
    `bright_white`, all `Option<String>`) so the frontend can map it 1:1 onto an
    `ITheme` object.
  - `TerminalCursorPreferences` — `active_shape` (enum: `Block`/`Underline`/`Bar`,
    matches xterm's `cursorStyle`), `inactive_shape` (enum: `Block`/`Underline`/
    `Bar`/`Outline`/`None`, matches xterm's `cursorInactiveStyle`), `blink: bool`,
    `color`/`accent_color: Option<String>` (hex).
  - `TerminalCompatibilityPreferences` — `backspace_key` and `delete_key`, each an
    enum `Bs`/`Del`/`Dch`.
  - `TerminalPreferences { text, cursor, compatibility }` — one new field on
    `UiConfig`: `pub terminal_preferences: TerminalPreferences` (`#[serde(default)]`).
- Remove `terminal_font_family`, `terminal_font_size`, `terminal_scrollback` from
  `UiConfig` and update `impl Default for UiConfig` accordingly.
- Add `get_terminal_preferences`/`set_terminal_preferences` `#[tauri::command]`s
  (whole-struct get/set, following the lock→mutate→clone→drop→`save_ui_config_to`
  choreography every other setter uses), replacing `get_terminal_scrollback`/
  `get_terminal_font`. Register in `lib.rs`'s `generate_handler!` list, removing the
  two commands being replaced.
- Update the two existing call sites in `TerminalPanel.tsx` (scrollback fetch, font
  fetch) to call the new combined command instead — required just to keep the app
  building/running through this unit, not the dialog itself (that's Work Unit 2).

### 2. Dialog shell + Text tab

New component, e.g. `debugger/frontend/src/TerminalPreferencesDialog.tsx`, following
the existing dialog pattern (`AboutDialog.tsx`/`NewProfileDialog.tsx`: local `open`
state, `modal-backdrop`/`modal-dialog` markup, `modal.scss` classes, manual
Escape/Enter `keydown` handling) since there's no shared `<Modal>` component to extend.
No tab-strip component exists either — build a minimal one (a row of buttons toggling
which section renders), styled to fit `modal.scss`'s existing look.

- Wire it open via a new `{ id: "preferences", text: "Preferences…" }` entry appended
  to the `items` array in `TerminalPanel.tsx`'s `openSizeMenu` (lines ~130-137), so
  it's reachable from both the right-click menu and the docked hamburger icon for
  free, same as the size presets.
- On open, fetch `get_terminal_preferences`; on Save, call `set_terminal_preferences`
  with the edited struct and apply the Text-tab-relevant values live to the terminal
  (`term.options.fontFamily`/`fontSize`/`scrollback`, and `term.options.theme` for
  foreground/background/16-palette — extending `TerminalPanel.tsx`'s existing
  `XTERM_DARK_THEME`/`XTERM_LIGHT_THEME` handling so a configured color overrides the
  theme default, `None` falls back to the current light/dark theme value as today).
  Cancel/Esc discards edits without calling the setter.
- Text tab controls: font family (text input, validated on Save via
  `isMonospaceFont` — an unknown or non-monospace font name shows an inline error
  next to the field and blocks Save/Enter until corrected, per the "Font face"
  decision above), font size (numeric input with +/- buttons),
  foreground/background color (the new color-swatch-popover component, built here and
  reused by Work Unit 3), 16-color palette (8 swatches, each togglable to its "bright"
  variant, or 16 swatches laid out 8+8 — implementer's call on layout), scrollback
  (numeric input).
- This unit is where the color-swatch/hex-input/popover component gets built (per the
  "Color chooser" decision above) — factor it as its own small component so Work Unit
  3's cursor/accent colors reuse it directly.

### 3. Cursor tab

- Active shape: maps directly to `cursorStyle` on the terminal, already a supported
  xterm option (`'block' | 'underline' | 'bar'`) — currently unset in
  `TerminalPanel.tsx` (defaults apply), so this is the first time it's driven by a
  preference at all.
- Inactive shape: maps directly to `cursorInactiveStyle`
  (`'outline' | 'block' | 'bar' | 'underline' | 'none'`), also unset today — same
  situation.
- Blink: checkbox maps to `cursorBlink` (boolean only, per the "Blink" decision above).
- Cursor color / accent color: reuse Work Unit 2's color-swatch-popover component;
  maps to `ITheme.cursor`/`ITheme.cursorAccent`, same override-the-theme-default
  pattern as Text tab's foreground/background.
- Apply on Save the same way as Work Unit 2 (`term.options.cursorStyle` etc., plus
  `term.options.theme` for the two colors).

### 4. Compatibility tab

- Two selects (or radio groups), one for Backspace, one for Delete, each offering the
  three `Bs`/`Del`/`Dch` options from Work Unit 1's enum.
- Implementation: `TerminalPanel.tsx` currently sends all keyboard input through
  xterm's own `onData` → `invoke("write_terminal", { bytes })` (line ~301). Backspace/
  Delete need to be intercepted *before* xterm's default encoding via
  `term.attachCustomKeyEventHandler`, matching the configured action instead of
  xterm's built-in encoding: ASCII 8 → `\x08`, ASCII 127 → `\x7f`, or the DCH escape
  sequence (`\x1b[3~` is VT220 Delete, not DCH — the issue specifically says "ANSI
  Delete Character (DCH) escape sequence," which is `CSI Pn P`, i.e. `\x1b[P`; use that
  literal sequence, not the VT220 Delete keycode, for the DCH option). The handler
  returns `false` to suppress xterm's own handling once it has written the chosen
  bytes via the same `invoke("write_terminal", ...)` path.
- Apply on Save by storing the two selected actions in a ref the custom key handler
  reads from (no live xterm option for this — it's plan-implemented behavior, not an
  xterm setting).

## Key files

- `debugger/src-tauri/src/preferences.rs` — `TerminalPreferences` and sub-structs,
  `UiConfig` field swap, `get_terminal_preferences`/`set_terminal_preferences`
  commands, tests
- `debugger/src-tauri/src/lib.rs` — command registration swap
- `debugger/frontend/src/TerminalPanel.tsx` — `openSizeMenu` new item, live-apply of
  saved preferences (theme/cursor options), custom key handler for Compatibility
- `debugger/frontend/src/TerminalPreferencesDialog.tsx` (new) — modal shell, tab strip,
  Save/Cancel/Enter/Esc, all three tabs' controls
- `debugger/frontend/src/terminalSizing.ts` — reused monospace-validation/fallback
  helpers for the font field
- `debugger/frontend/src/styles/modal.scss` — extended with tab-strip and
  color-swatch-popover styles, reusing existing modal token classes

## Verification

- `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace
  --all-targets` clean after Work Unit 1; `npm run build` (tsc + vite) clean after
  every unit.
- Run the debugger (`cargo tauri dev`) against the default TaliForth profile for each
  UI-facing unit (2-4): open Preferences from both the right-click menu and the docked
  hamburger icon; confirm Save persists (re-open dialog, or restart the app, and see
  the same values); confirm Cancel/Esc discards edits; confirm Enter saves.
- Work Unit 2: change font family/size, scrollback, foreground/background, and a
  couple of palette colors; confirm the live terminal reflects each after Save.
- Work Unit 3: cycle through active/inactive cursor shapes and blink; confirm the
  visible cursor changes; set cursor/accent colors and confirm they render.
- Work Unit 4: for both Backspace and Delete, try all three compatibility options and
  confirm the emulated program (or a simple `stty`/raw-echo check) receives the
  expected byte(s) — `\x08`, `\x7f`, or `\x1b[P`.
- Confirm light/dark theme switching still works correctly with preference-overridden
  colors (override wins in both modes; unset fields still follow the theme).

## Workflow

This plan is implemented as four sequential units of work (numbered above).
For each: create a branch named for the unit, do the work plus the
validation described above, commit, push to origin, and open a PR calling
out any manual UAT needed. Await explicit instruction before merging that PR
or starting the next unit.
