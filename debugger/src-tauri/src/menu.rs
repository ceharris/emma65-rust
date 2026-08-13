//! Native application menu bar: File/Edit/View/Run/Memory/Window/Help.

use std::path::PathBuf;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, State, Wry};

/// Menu item id for the Window > Terminal item.
pub(crate) const TOGGLE_TERMINAL_ID: &str = "toggle-terminal";
/// Menu item id for the Window > Restore Layout… item.
pub(crate) const RESTORE_LAYOUT_ID: &str = "restore-layout";
/// Id prefix for entries in the View menu; each item's full id is this
/// prefix followed by the panel's dockview id (`MainPanelId` in
/// `panelRegistry.tsx`), e.g. `"view-panel:trace"`.
pub(crate) const VIEW_PANEL_ID_PREFIX: &str = "view-panel:";
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

/// Builds the native app menu (File/Edit/View/Run/Memory/Window/Help) and the `exit_item`
/// handle `on_menu_event` needs to dispatch an Exit click. The File > Open
/// Recent submenu starts empty — populated once the recent-profiles list is
/// loaded, via `rebuild_open_recent_submenu`.
#[allow(clippy::type_complexity)]
pub fn build_menu(
    app: &tauri::App,
) -> tauri::Result<(Menu<Wry>, WindowMenuState, RecentMenuState, RunMenuState, MemoryMenuState)> {
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

    // Placeholder: no items yet.
    let edit_menu = Submenu::new(app, "Edit", true)?;

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
    // (issue #393), and Terminal's Ctrl+Shift+T remains the Window menu's
    // detach/attach accelerator below, so reusing it here would collide.
    let view_panels: [(&str, &str); 10] = [
        ("breakpoints", "Breakpoints"),
        ("disassembly", "Disassembly"),
        ("log", "Log"),
        ("memory", "Memory"),
        ("registers", "Registers"),
        ("run-controls", "Run Controls"),
        ("stack", "Stack"),
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

    // Bottom of the Window menu, set off by its own separator (issue #398):
    // discards every panel's current position/size, restoring the same
    // default arrangement a brand-new profile starts with. Dialog-confirmed
    // like Clear Recent… above — `on_menu_event` in `lib.rs` just opens the
    // confirmation modal; the actual reset happens via the `restore_dock_layout`
    // command once the user confirms (see `layout.rs`).
    let window_separator = PredefinedMenuItem::separator(app)?;
    let restore_layout_item = MenuItem::with_id(app, RESTORE_LAYOUT_ID, "Restore Layout…", true, None::<&str>)?;
    let window_menu =
        Submenu::with_items(app, "Window", true, &[&terminal_item, &window_separator, &restore_layout_item])?;

    let github_item = MenuItem::with_id(app, GITHUB_ID, "View on GitHub", true, None::<&str>)?;
    let help_separator = PredefinedMenuItem::separator(app)?;
    // A plain `MenuItem` rather than `PredefinedMenuItem::about`: the
    // predefined item delegates to the OS/toolkit's own About box (bare and
    // unstyled on Linux/GTK), which can't carry the app description,
    // copyright, license, and production-only build-info line issue #423
    // asks for. `on_menu_event` in `lib.rs` opens the same kind of custom
    // modal dialog every other menu item here does.
    let about_item = MenuItem::with_id(app, ABOUT_ID, "About", true, None::<&str>)?;
    let help_menu = Submenu::with_items(app, "Help", true, &[&github_item, &help_separator, &about_item])?;

    let menu = Menu::with_items(
        app,
        &[&file_menu, &edit_menu, &view_menu, &run_menu, &memory_menu, &window_menu, &help_menu],
    )?;

    Ok((
        menu,
        WindowMenuState { exit_item, terminal_item },
        RecentMenuState(open_recent_submenu),
        RunMenuState { run_item, stop_item, step_into_item, step_over_item, step_return_item, toggle_auto_step_item },
        MemoryMenuState {
            load_item: load_memory_item,
            save_item: save_memory_item,
            edit_item: edit_memory_item,
            fill_item: fill_memory_item,
        },
    ))
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
