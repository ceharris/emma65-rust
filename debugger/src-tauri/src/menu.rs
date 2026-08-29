//! Native application menu bar: File/Edit/View/Run/Memory/Window/Help.

use std::path::PathBuf;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, State, Wry};

/// Menu item id for the Window > Terminal item.
pub(crate) const TOGGLE_TERMINAL_ID: &str = "toggle-terminal";
/// Menu item id for the Window > Display item.
pub(crate) const TOGGLE_DISPLAY_ID: &str = "toggle-display";
/// Menu item id for the Window > LED Matrix item.
pub(crate) const TOGGLE_LED_MATRIX_ID: &str = "toggle-led-matrix";
/// Menu item id for the Window > Restore Layout… item.
pub(crate) const RESTORE_LAYOUT_ID: &str = "restore-layout";
/// Id prefix for entries in the View menu; each item's full id is this
/// prefix followed by the panel's dockview id (`MainPanelId` in
/// `panelRegistry.tsx`), e.g. `"view-panel:trace"`.
pub(crate) const VIEW_PANEL_ID_PREFIX: &str = "view-panel:";
/// Menu item id / `edit-menu-action` event payload for the Edit > Cut item.
pub(crate) const CUT_ID: &str = "cut";
/// Menu item id / `edit-menu-action` event payload for the Edit > Copy item.
pub(crate) const COPY_ID: &str = "copy";
/// Menu item id / `edit-menu-action` event payload for the Edit > Paste item.
pub(crate) const PASTE_ID: &str = "paste";

/// Menu item id for the File > New Profile item.
pub(crate) const NEW_PROFILE_ID: &str = "new-profile";
/// Menu item id for the File > Open Profile item.
pub(crate) const OPEN_PROFILE_ID: &str = "open-profile";
/// Id prefix for entries in the File > Open Recent submenu; each item's full
/// id is this prefix followed by the profile's absolute directory path.
pub(crate) const OPEN_RECENT_ID_PREFIX: &str = "open-recent:";
/// Menu item id for the "Clear Recent…" item at the bottom of the File >
/// Open Recent submenu.
pub(crate) const CLEAR_RECENT_ID: &str = "clear-recent";
/// Menu item id for the File > Exit item.
pub(crate) const EXIT_ID: &str = "exit";

/// Menu item id for the Help > About item.
pub(crate) const ABOUT_ID: &str = "about";
/// Menu item id for the Help > View on GitHub item.
pub(crate) const GITHUB_ID: &str = "github";
/// URL opened in the user's browser by the Help > View on GitHub item.
pub(crate) const GITHUB_REPO_URL: &str = "https://github.com/ceharris/emma65-rust";

/// Menu item id / `run-menu-action` event payload for the Run > Run item.
pub(crate) const RUN_CPU_ID: &str = "run-cpu";
/// Menu item id / `run-menu-action` event payload for the Run > Stop item.
pub(crate) const STOP_CPU_ID: &str = "stop-cpu";
/// Menu item id / `run-menu-action` event payload for the Run > Step Into item.
pub(crate) const STEP_INTO_ID: &str = "step-into";
/// Menu item id / `run-menu-action` event payload for the Run > Step Over item.
pub(crate) const STEP_OVER_ID: &str = "step-over";
/// Menu item id / `run-menu-action` event payload for the Run > Step Return item.
pub(crate) const STEP_RETURN_ID: &str = "step-return";
/// Menu item id / `run-menu-action` event payload for the Run > Toggle Auto-Step item.
pub(crate) const TOGGLE_AUTO_STEP_ID: &str = "toggle-auto-step";

/// Menu item id / `memory-menu-action` event payload for the Memory > Load from File… item.
pub(crate) const LOAD_MEMORY_ID: &str = "load-memory";
/// Menu item id / `memory-menu-action` event payload for the Memory > Save to File… item.
pub(crate) const SAVE_MEMORY_ID: &str = "save-memory";
/// Menu item id / `memory-menu-action` event payload for the Memory > Edit… item.
pub(crate) const EDIT_MEMORY_ID: &str = "edit-memory";
/// Menu item id / `memory-menu-action` event payload for the Memory > Fill… item.
pub(crate) const FILL_MEMORY_ID: &str = "fill-memory";

/// Menu item id / `assembler-menu-action` event payload for the Assembler > New item.
pub(crate) const NEW_ASSEMBLER_ID: &str = "new-assembler";
/// Menu item id / `assembler-menu-action` event payload for the Assembler > Open… item.
pub(crate) const OPEN_ASSEMBLER_ID: &str = "open-assembler";
/// Menu item id / `assembler-menu-action` event payload for the Assembler > Save item.
pub(crate) const SAVE_ASSEMBLER_ID: &str = "save-assembler";
/// Menu item id / `assembler-menu-action` event payload for the Assembler > Save As… item.
pub(crate) const SAVE_AS_ASSEMBLER_ID: &str = "save-as-assembler";
/// Menu item id / `assembler-menu-action` event payload for the Assembler > Assemble & Load item.
pub(crate) const ASSEMBLE_LOAD_ID: &str = "assemble-load";

/// Holds menu items `on_menu_event`/other modules need a handle to after
/// construction. The View menu's per-panel items (issue #393) are plain,
/// non-checkable items matched by id prefix alone in `on_menu_event`, so no
/// handle to them needs to be kept here. Terminal (since #385) toggles
/// between "Detach Terminal…" and "Attach Terminal" depending on whether
/// it's currently docked or detached, so its handle is kept for
/// `set_terminal_menu_label` to mutate in place.
pub struct WindowMenuState {
    /// The File > Exit item.
    pub exit_item: MenuItem<Wry>,
    /// The Window > Terminal item.
    pub terminal_item: MenuItem<Wry>,
    /// The Window > Display item. Toggles between "Detach Display…" and "Attach Display" the
    /// same way `terminal_item` does — see `set_display_menu_label`.
    pub display_item: MenuItem<Wry>,
    /// The Window > LED Matrix item. Toggles between "Detach LED Matrix…" and "Attach LED
    /// Matrix" the same way `terminal_item` does — see `set_led_matrix_menu_label`.
    pub led_matrix_item: MenuItem<Wry>,
}

/// Holds the File > Open Recent submenu so its items can be replaced in
/// place whenever the recent-profiles list changes (see
/// `recent::record_recent_profile`), without rebuilding the whole app menu.
pub struct RecentMenuState(pub Submenu<Wry>);

/// Holds the Run menu's six item handles so `set_run_controls_enabled`
/// (issue #395) can toggle each one's enabled state in place, mirroring the
/// floating Run Controls panel's own button `disabled` logic.
pub struct RunMenuState {
    /// The Run > Run item.
    pub run_item: MenuItem<Wry>,
    /// The Run > Stop item.
    pub stop_item: MenuItem<Wry>,
    /// The Run > Step Into item.
    pub step_into_item: MenuItem<Wry>,
    /// The Run > Step Over item.
    pub step_over_item: MenuItem<Wry>,
    /// The Run > Step Return item.
    pub step_return_item: MenuItem<Wry>,
    /// The Run > Toggle Auto-Step item.
    pub toggle_auto_step_item: MenuItem<Wry>,
}

/// Enabled state for the Run menu's six items, pushed from the frontend
/// (`RunControlsContext.tsx`) whenever the underlying run/step/auto-step
/// state changes — the same booleans that drive the floating Run Controls
/// panel's own button `disabled` attributes, kept in sync so the native menu
/// never lets a click through that the panel itself would have blocked.
#[derive(serde::Deserialize)]
pub struct RunControlsEnabled {
    /// Whether Run > Run should be enabled.
    pub run: bool,
    /// Whether Run > Stop should be enabled.
    pub stop: bool,
    /// Whether Run > Step Into should be enabled.
    pub step_into: bool,
    /// Whether Run > Step Over should be enabled.
    pub step_over: bool,
    /// Whether Run > Step Return should be enabled.
    pub step_return: bool,
    /// Whether Run > Toggle Auto-Step should be enabled.
    pub toggle_auto_step: bool,
}

/// Holds the Edit menu's three item handles so `set_edit_menu_enabled` can
/// toggle each one's enabled state in place, mirroring `RunMenuState`. Unlike
/// the Run menu, there's no corresponding UI panel with its own `disabled`
/// buttons to mirror — enabled state here instead tracks where the frontend's
/// focus/selection currently is (`EditMenuContext.tsx`), since Cut/Copy/Paste
/// only make sense against a specific editable or selectable target.
pub struct EditMenuState {
    /// The Edit > Cut item.
    pub cut_item: MenuItem<Wry>,
    /// The Edit > Copy item.
    pub copy_item: MenuItem<Wry>,
    /// The Edit > Paste item.
    pub paste_item: MenuItem<Wry>,
}

/// Enabled state for the Edit menu's three items, pushed from the frontend
/// (`EditMenuContext.tsx`) whenever the currently focused/selected target
/// changes.
#[derive(serde::Deserialize)]
pub struct EditMenuEnabled {
    /// Whether Edit > Cut should be enabled.
    pub cut: bool,
    /// Whether Edit > Copy should be enabled.
    pub copy: bool,
    /// Whether Edit > Paste should be enabled.
    pub paste: bool,
}

/// Holds the Memory menu's four item handles so `set_memory_menu_enabled`
/// (issue #411) can toggle them all in place — all four share a single
/// enabled condition (the CPU must be stopped), unlike the Run menu's six
/// independently-gated items, so there's no need for a per-item flags struct
/// here.
pub struct MemoryMenuState {
    /// The Memory > Load from File… item.
    pub load_item: MenuItem<Wry>,
    /// The Memory > Save to File… item.
    pub save_item: MenuItem<Wry>,
    /// The Memory > Edit… item.
    pub edit_item: MenuItem<Wry>,
    /// The Memory > Fill… item.
    pub fill_item: MenuItem<Wry>,
}

/// Holds the Assembler > Assemble & Load item handle so
/// `set_assembler_menu_enabled` can toggle it in place, mirroring
/// `MemoryMenuState`. New/Open…/Save/Save As… don't depend on CPU state
/// (they only touch the in-panel editor buffer and the filesystem), so
/// unlike `MemoryMenuState`'s four items, only this one needs tracking here.
pub struct AssemblerMenuState {
    /// The Assembler > Assemble & Load item.
    pub assemble_load_item: MenuItem<Wry>,
}

/// Builds the native app menu (File/Edit/View/Run/Memory/Window/Help) and the `exit_item`
/// handle `on_menu_event` needs to dispatch an Exit click. The File > Open
/// Recent submenu starts empty — populated once the recent-profiles list is
/// loaded, via `rebuild_open_recent_submenu`.
#[allow(clippy::type_complexity)]
pub fn build_menu(
    app: &tauri::App,
) -> tauri::Result<(Menu<Wry>, WindowMenuState, RecentMenuState, RunMenuState, MemoryMenuState, AssemblerMenuState, EditMenuState)> {
    // A plain `MenuItem` rather than `PredefinedMenuItem::quit`: muda's GTK
    // backend silently drops `Quit` (it isn't in its short list of supported
    // predefined types on Linux), so the item never appeared at all. The
    // click is dispatched to the same `request_exit` the "quit" command
    // (bound to Ctrl+Q — see `App.tsx`) already uses. Both the menu
    // accelerator and the JS-level binding are main-window-only (issue
    // #351): the main window is the only one carrying this menu, and Ctrl+Q
    // is handled locally in `App.tsx` rather than via the cross-window
    // `APP_KEY_BINDINGS` array.
    let new_profile_item = MenuItem::with_id(app, NEW_PROFILE_ID, "New Profile", true, Some("CmdOrCtrl+N"))?;
    let open_profile_item = MenuItem::with_id(app, OPEN_PROFILE_ID, "Open Profile", true, Some("CmdOrCtrl+O"))?;
    let open_recent_submenu = Submenu::with_id(app, "open-recent", "Open Recent", false)?;
    let exit_item = MenuItem::with_id(app, EXIT_ID, "Exit", true, Some("CmdOrCtrl+Q"))?;
    let separator = PredefinedMenuItem::separator(app)?;
    let file_menu = Submenu::with_items(
        app,
        "File",
        true,
        &[&new_profile_item, &open_profile_item, &open_recent_submenu, &separator, &exit_item],
    )?;

    // Plain `MenuItem`s rather than `PredefinedMenuItem::cut/copy/paste`:
    // muda's GTK backend only wires those up through the optional `libxdo`
    // feature (X11-only — its own source marks Wayland `// TODO`), which
    // isn't enabled here, so on this app's Linux target they'd be silent
    // no-ops — the same class of problem that ruled out
    // `PredefinedMenuItem::quit` for the File menu above. `EditMenuContext.tsx`
    // tracks enabled state (via `set_edit_menu_enabled` below) and performs
    // the actual cut/copy/paste in response to `on_menu_event` dispatching
    // `edit-menu-action` — see that module for why a JS-side context, not
    // more Rust here, owns the focus/selection tracking.
    //
    // No accelerators: `CmdOrCtrl+C` here was tried and confirmed (manual
    // testing, issue #435) to intercept Ctrl+C before it reaches the
    // terminal, breaking its interrupt (SIGINT) key — this accelerator is
    // registered on the native GTK window, a separate mechanism from
    // WebKitGTK's own in-page key handling that wins the conflict. Terminal
    // already binds its own copy/paste to Ctrl+Shift+C/V specifically to
    // stay clear of the terminal-control-character combos (see
    // `TerminalPanel.tsx`), but muda's accelerator API has no way to scope
    // an accelerator to "everywhere except Terminal", and Ctrl+Shift+C/V
    // would be a confusing combo to advertise for Cut/Copy/Paste app-wide.
    // So these are mouse/menu-driven only, same as most of this file's
    // other custom items (e.g. `restore_layout_item` below).
    let cut_item = MenuItem::with_id(app, CUT_ID, "Cut", false, None::<&str>)?;
    let copy_item = MenuItem::with_id(app, COPY_ID, "Copy", false, None::<&str>)?;
    let paste_item = MenuItem::with_id(app, PASTE_ID, "Paste", false, None::<&str>)?;
    let edit_menu = Submenu::with_items(app, "Edit", true, &[&cut_item, &copy_item, &paste_item])?;

    // One item per dockable panel (issue #393), letting a user bring back a
    // panel whose dock tab they closed — the old Window-menu "Reveal…" items
    // this replaces only ever re-activated an already-present tab, so they
    // had no effect once a panel was actually dismissed (see `on_menu_event`
    // in `lib.rs`, which now handles both cases via the shared `reveal-panel`
    // event). Ordered lexically by the label shown on the panel's dock tab
    // (`PANEL_TITLES` in `panelRegistry.tsx`), not by dock position — kept in
    // sync with that map by hand, since Rust has no access to the frontend's
    // TypeScript constants. None of these carry an accelerator: Trace/Log's
    // former Ctrl+Shift+Y/L bindings were dropped rather than reassigned here
    // (issue #393), and Terminal's Ctrl+Shift+T and Display's Ctrl+Shift+D
    // remain the Window menu's detach/attach accelerators below, so reusing
    // either here would collide.
    let view_panels: [(&str, &str); 14] = [
        ("assembler", "Assembler"),
        ("breakpoints", "Breakpoints"),
        ("disassembly", "Disassembly"),
        ("display", "Display"),
        ("led-matrix", "LED Matrix"),
        ("log", "Log"),
        ("memory", "Memory"),
        ("registers", "Registers"),
        ("run-controls", "Run Controls"),
        ("stack", "Stack"),
        ("symbols", "Symbols"),
        ("terminal", "Terminal"),
        ("trace", "Trace"),
        ("watchpoints", "Watchpoints"),
    ];
    let view_menu = Submenu::new(app, "View", true)?;
    for (id, label) in view_panels {
        let item = MenuItem::with_id(app, format!("{VIEW_PANEL_ID_PREFIX}{id}"), label, true, None::<&str>)?;
        view_menu.append(&item)?;
    }

    // One item per Run/Step action (issue #395), mirroring the floating Run
    // Controls panel's five buttons plus its Auto-Step toggle. F5/F10/F11 and
    // their Shift variants are all valid muda accelerators on every platform
    // — unlike Ctrl+Shift+T above, F-keys have no GTK shift-consumption issue
    // since they aren't punctuation/digit keys. `on_menu_event` in `lib.rs`
    // both reveals the floating panel (the existing `reveal-panel` event) and
    // dispatches the action itself (a new `run-menu-action` event) on a
    // click, so a menu click, a native accelerator, and the panel's own
    // button all funnel through the exact same handler in
    // `RunControlsContext.tsx`. Enabled state is pushed from there via
    // `set_run_controls_enabled`, not tracked here.
    let run_item = MenuItem::with_id(app, RUN_CPU_ID, "Run", true, Some("F5"))?;
    let stop_item = MenuItem::with_id(app, STOP_CPU_ID, "Stop", true, Some("Shift+F5"))?;
    let step_into_item = MenuItem::with_id(app, STEP_INTO_ID, "Step Into", true, Some("F11"))?;
    let step_over_item = MenuItem::with_id(app, STEP_OVER_ID, "Step Over", true, Some("F10"))?;
    let step_return_item = MenuItem::with_id(app, STEP_RETURN_ID, "Step Return", true, Some("Shift+F11"))?;
    let toggle_auto_step_item =
        MenuItem::with_id(app, TOGGLE_AUTO_STEP_ID, "Toggle Auto-Step", true, Some("CmdOrCtrl+Shift+F5"))?;
    let run_menu = Submenu::with_items(
        app,
        "Run",
        true,
        &[&run_item, &stop_item, &step_into_item, &step_over_item, &step_return_item, &toggle_auto_step_item],
    )?;

    // Replaces the Memory panel's own header button row (issue #411) with a
    // top-level menu, following the same click-dispatches-an-event pattern as
    // the Run menu above: `on_menu_event` in `lib.rs` reveals the Memory
    // panel then emits `memory-menu-action`, which `MemoryPanel.tsx` handles
    // the same way it handles a click on its own dialog-opening code (there
    // are no more buttons left to click). The four former shortcuts
    // Alt+Shift+H/Alt+Shift+A (open Edit, hex/UTF-8) and Alt+F/Alt+Shift+F
    // (Load/Fill) are discarded outright rather than remapped, since Edit no
    // longer needs two separate entry points now that its dialog carries its
    // own Hexadecimal/ASCII-Unicode Text radio group.
    let load_memory_item = MenuItem::with_id(app, LOAD_MEMORY_ID, "Load from File…", true, Some("CmdOrCtrl+L"))?;
    let save_memory_item = MenuItem::with_id(app, SAVE_MEMORY_ID, "Save to File…", true, Some("CmdOrCtrl+S"))?;
    let edit_memory_item = MenuItem::with_id(app, EDIT_MEMORY_ID, "Edit…", true, Some("CmdOrCtrl+Shift+E"))?;
    let fill_memory_item = MenuItem::with_id(app, FILL_MEMORY_ID, "Fill…", true, Some("CmdOrCtrl+Shift+F"))?;
    let memory_separator_1 = PredefinedMenuItem::separator(app)?;
    let memory_separator_2 = PredefinedMenuItem::separator(app)?;
    let memory_menu = Submenu::with_items(
        app,
        "Memory",
        true,
        &[
            &load_memory_item,
            &save_memory_item,
            &memory_separator_1,
            &edit_memory_item,
            &memory_separator_2,
            &fill_memory_item,
        ],
    )?;

    // Gets a real native accelerator (issue #377), same as the File-menu items
    // above, so the shortcut text renders with native styling instead of being baked
    // into the label. The accelerator only ever fires while the main window (the only
    // one carrying this menu) has focus; `on_menu_event` in `lib.rs` handles the click.
    // The same combo also works via the JS-level listener in `useAppKeyBindings.ts`
    // (needed today for the detached Terminal window, and for any future window that
    // installs that hook) — that listener skips it specifically for the main window,
    // so the native accelerator and the JS invoke don't both fire there.
    //
    // This is Ctrl+Shift+T rather than the VS Code-style Ctrl+Shift+` originally
    // wanted (issue #377): on GTK, Shift is a *consumed* modifier for punctuation keys
    // like backtick or digits — it's what selects the shifted symbol (backtick+Shift is
    // "~", 1+Shift is "!") — so registering the accelerator as the unshifted symbol plus
    // a Shift requirement never matches the actual key-press GTK reports, which arrives
    // as the already-shifted character with Shift folded in. Tauri's menu API only
    // accepts a physical-key accelerator string and always builds it from the unshifted
    // symbol, so there's no accelerator string that works around this for punctuation
    // keys. Letters don't have the problem, since GTK canonicalizes a shifted letter to
    // its uppercase keyval, which is exactly how muda registers it — hence the fallback
    // to a letter here. See https://docs.gtk.org/gtk3/class.AccelGroup.html on consumed
    // modifiers.
    //
    // Terminal can be docked *or* detached to its own window, so its click toggles
    // between the two — starts out "Detach Terminal…", mutated to "Attach Terminal" in
    // place by `set_terminal_menu_label` whenever that state flips (including once here
    // at startup, if the persisted layout says Terminal was left detached). Bringing a
    // dismissed-while-docked Terminal tab back (issue #393) is instead View > Terminal's
    // job, alongside the other eight panels — see `on_menu_event` in `lib.rs`.
    let terminal_item = MenuItem::with_id(app, TOGGLE_TERMINAL_ID, "Detach Terminal…", true, Some("Ctrl+Shift+T"))?;

    // Same toggle-label pattern as `terminal_item` above (memory-mapped display device plan,
    // Work Unit 4), with its own accelerator letter (`D`) to avoid colliding with Terminal's —
    // same GTK consumed-modifier reasoning documented on `terminal_item` rules out a
    // punctuation-based combo here too.
    let display_item = MenuItem::with_id(app, TOGGLE_DISPLAY_ID, "Detach Display…", true, Some("Ctrl+Shift+D"))?;

    // Same toggle-label pattern as `terminal_item`/`display_item` above (memory-mapped LED
    // matrix device plan, Work Unit 4), with its own accelerator letter (`M`) to avoid colliding
    // with Terminal's/Display's — same GTK consumed-modifier reasoning documented on
    // `terminal_item` rules out a punctuation-based combo here too.
    let led_matrix_item =
        MenuItem::with_id(app, TOGGLE_LED_MATRIX_ID, "Detach LED Matrix…", true, Some("Ctrl+Shift+M"))?;

    // Bottom of the Window menu, set off by its own separator (issue #398):
    // discards every panel's current position/size, restoring the same
    // default arrangement a brand-new profile starts with. Dialog-confirmed
    // like Clear Recent… above — `on_menu_event` in `lib.rs` just opens the
    // confirmation modal; the actual reset happens via the `restore_dock_layout`
    // command once the user confirms (see `layout.rs`).
    let window_separator = PredefinedMenuItem::separator(app)?;
    let restore_layout_item = MenuItem::with_id(app, RESTORE_LAYOUT_ID, "Restore Layout…", true, None::<&str>)?;
    let window_menu = Submenu::with_items(
        app,
        "Window",
        true,
        &[&terminal_item, &display_item, &led_matrix_item, &window_separator, &restore_layout_item],
    )?;

    let github_item = MenuItem::with_id(app, GITHUB_ID, "View on GitHub", true, None::<&str>)?;
    // A plain `MenuItem` rather than `PredefinedMenuItem::about`: the
    // predefined item delegates to the OS/toolkit's own About box (bare and
    // unstyled on Linux/GTK), which can't carry the app description,
    // copyright, license, and production-only build-info line issue #423
    // asks for. `on_menu_event` in `lib.rs` opens the same kind of custom
    // modal dialog every other menu item here does.
    let about_item = MenuItem::with_id(app, ABOUT_ID, "About", true, None::<&str>)?;
    let help_menu = Submenu::with_items(app, "Help", true, &[&github_item, &about_item])?;

    // Replaces the Assembler panel's own header button (issue #474, debugger
    // integration Unit 4) with a top-level menu, same click-dispatches-an-event
    // pattern as the Memory menu above: `on_menu_event` in `lib.rs` reveals the
    // Assembler panel then emits `assembler-menu-action`, which
    // `AssemblerPanel.tsx` handles — it owns all the actual file-dialog/dirty-
    // tracking logic, there being no separate profile-scoped default source
    // file the way Memory's binary images have. Accelerators deliberately use
    // `Alt` rather than `CmdOrCtrl(+Shift)` to keep this namespace
    // visually/mechanically distinct from Memory's — confirmed no `Alt+`-
    // modified accelerator exists anywhere else in this file, so this is a
    // fresh, collision-free namespace. Assemble & Load's `F9` is shared
    // between this menu and the panel's own header button from Unit 3, both
    // funneled through `AssemblerPanel.tsx`'s single `runAssemble` handler.
    let new_assembler_item = MenuItem::with_id(app, NEW_ASSEMBLER_ID, "New", true, Some("Alt+N"))?;
    let open_assembler_item = MenuItem::with_id(app, OPEN_ASSEMBLER_ID, "Open…", true, Some("Alt+O"))?;
    let save_assembler_item = MenuItem::with_id(app, SAVE_ASSEMBLER_ID, "Save", true, Some("Alt+S"))?;
    let save_as_assembler_item = MenuItem::with_id(app, SAVE_AS_ASSEMBLER_ID, "Save As…", true, None::<&str>)?;
    let assembler_separator = PredefinedMenuItem::separator(app)?;
    let assemble_load_item = MenuItem::with_id(app, ASSEMBLE_LOAD_ID, "Assemble…", true, Some("F9"))?;
    let assembler_menu = Submenu::with_items(
        app,
        "Assembler",
        true,
        &[
            &new_assembler_item,
            &open_assembler_item,
            &save_assembler_item,
            &save_as_assembler_item,
            &assembler_separator,
            &assemble_load_item,
        ],
    )?;

    let menu = Menu::with_items(
        app,
        &[&file_menu, &edit_menu, &view_menu, &run_menu, &memory_menu, &assembler_menu, &window_menu, &help_menu],
    )?;

    Ok((
        menu,
        WindowMenuState { exit_item, terminal_item, display_item, led_matrix_item },
        RecentMenuState(open_recent_submenu),
        RunMenuState { run_item, stop_item, step_into_item, step_over_item, step_return_item, toggle_auto_step_item },
        MemoryMenuState {
            load_item: load_memory_item,
            save_item: save_memory_item,
            edit_item: edit_memory_item,
            fill_item: fill_memory_item,
        },
        AssemblerMenuState { assemble_load_item },
        EditMenuState { cut_item, copy_item, paste_item },
    ))
}

/// Pushes `flags` onto the Edit menu's three items' enabled state
/// (`EditMenuContext.tsx` calls this whenever the currently focused/selected
/// target changes), so a menu click never fires against a context that
/// doesn't support it.
#[tauri::command]
pub fn set_edit_menu_enabled(flags: EditMenuEnabled, state: State<EditMenuState>) {
    let _ = state.cut_item.set_enabled(flags.cut);
    let _ = state.copy_item.set_enabled(flags.copy);
    let _ = state.paste_item.set_enabled(flags.paste);
}

/// Enables or disables all four Memory menu items together — they share a
/// single condition (the CPU must be stopped), matching the `disabled`
/// attribute the panel's old header buttons carried before issue #411
/// replaced them with this menu. `MemoryPanel.tsx` calls this whenever
/// `execState` changes, the same way `RunControlsContext.tsx` keeps the Run
/// menu's items in lockstep with the floating Run Controls panel.
#[tauri::command]
pub fn set_memory_menu_enabled(enabled: bool, state: State<MemoryMenuState>) {
    let _ = state.load_item.set_enabled(enabled);
    let _ = state.save_item.set_enabled(enabled);
    let _ = state.edit_item.set_enabled(enabled);
    let _ = state.fill_item.set_enabled(enabled);
}

/// Enables or disables the Assembler menu's Assemble & Load item — the CPU
/// must be stopped, mirroring the panel's own header button
/// (`usePanelHeaderAction`'s `disabled={execState !== "stopped"}` in
/// `AssemblerPanel.tsx`). New/Open…/Save/Save As… aren't gated by CPU state,
/// so unlike `set_memory_menu_enabled` this only touches one item.
#[tauri::command]
pub fn set_assembler_menu_enabled(enabled: bool, state: State<AssemblerMenuState>) {
    let _ = state.assemble_load_item.set_enabled(enabled);
}

/// Pushes `flags` onto the Run menu's six items' enabled state
/// (`RunControlsContext.tsx` calls this whenever the underlying run/step/
/// auto-step state changes), so a native accelerator or menu click never
/// gets through when the floating Run Controls panel's matching button would
/// have been disabled.
#[tauri::command]
pub fn set_run_controls_enabled(flags: RunControlsEnabled, state: State<RunMenuState>) {
    let _ = state.run_item.set_enabled(flags.run);
    let _ = state.stop_item.set_enabled(flags.stop);
    let _ = state.step_into_item.set_enabled(flags.step_into);
    let _ = state.step_over_item.set_enabled(flags.step_over);
    let _ = state.step_return_item.set_enabled(flags.step_return);
    let _ = state.toggle_auto_step_item.set_enabled(flags.toggle_auto_step);
}

/// Updates the Window > Terminal item's label to reflect whether the
/// terminal is currently detached to its own window ("Attach Terminal") or
/// docked ("Detach Terminal…") — mutated in place, the same pattern
/// `rebuild_open_recent_submenu` uses for its submenu, rather than
/// rebuilding the whole app menu.
pub(crate) fn set_terminal_menu_label(state: &WindowMenuState, detached: bool) {
    let label = if detached { "Attach Terminal" } else { "Detach Terminal…" };
    let _ = state.terminal_item.set_text(label);
}

/// Updates the Window > Display item's label the same way `set_terminal_menu_label` does for
/// Terminal.
pub(crate) fn set_display_menu_label(state: &WindowMenuState, detached: bool) {
    let label = if detached { "Attach Display" } else { "Detach Display…" };
    let _ = state.display_item.set_text(label);
}

/// Updates the Window > LED Matrix item's label the same way `set_terminal_menu_label` does for
/// Terminal.
pub(crate) fn set_led_matrix_menu_label(state: &WindowMenuState, detached: bool) {
    let label = if detached { "Attach LED Matrix" } else { "Detach LED Matrix…" };
    let _ = state.led_matrix_item.set_text(label);
}

/// Replaces the File > Open Recent submenu's items with `entries` (each a
/// display label paired with the profile's absolute directory path) followed
/// by a separator and a "Clear Recent…" item, provided `has_recent` is true
/// — the submenu (and, when there are no entries to show, just the "Clear
/// Recent…" item on its own) is otherwise left empty and disabled.
///
/// `has_recent` is driven by whether the stored recent-profiles list is
/// non-empty, not by whether `entries` is — the active profile is always
/// filtered out of `entries` before this is called, so a list containing
/// only the active profile still leaves something to clear.
///
/// muda's `Submenu` supports mutating items after construction, so this
/// updates the existing submenu in place rather than rebuilding the whole
/// app menu via `app.set_menu()`.
pub(crate) fn rebuild_open_recent_submenu(
    app: &AppHandle,
    state: &RecentMenuState,
    entries: &[(String, PathBuf)],
    has_recent: bool,
) -> tauri::Result<()> {
    let submenu = &state.0;
    let existing = submenu.items()?.len();
    for _ in 0..existing {
        submenu.remove_at(0)?;
    }
    for (label, path) in entries {
        let id = format!("{OPEN_RECENT_ID_PREFIX}{}", path.display());
        let item = MenuItem::with_id(app, id, label, true, None::<&str>)?;
        submenu.append(&item)?;
    }
    if has_recent {
        if !entries.is_empty() {
            submenu.append(&PredefinedMenuItem::separator(app)?)?;
        }
        let clear_item = MenuItem::with_id(app, CLEAR_RECENT_ID, "Clear Recent…", true, None::<&str>)?;
        submenu.append(&clear_item)?;
    }
    submenu.set_enabled(has_recent)?;
    Ok(())
}
