//! Native application menu bar: File/Edit/Window/Help.

use std::path::PathBuf;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, Wry};

use crate::terminal::TERMINAL_WINDOW_LABEL;
use crate::trace::TRACE_WINDOW_LABEL;

/// Menu item id for the Window > Terminal checkable item.
pub(crate) const TOGGLE_TERMINAL_ID: &str = "toggle-terminal";
/// Menu item id for the Window > Trace checkable item.
pub(crate) const TOGGLE_TRACE_ID: &str = "toggle-trace";
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

/// The menu items whose state needs to stay in sync with actual window
/// visibility, and (for Exit) whose click needs to be dispatched to app
/// logic. Managed as app state so both the global `on_menu_event` handler
/// and the `toggle_terminal_visibility`/`toggle_trace_visibility` commands
/// can reach the same item instances.
pub struct WindowMenuState {
    /// The Window > Terminal checkable item.
    pub terminal_item: CheckMenuItem<Wry>,
    /// The Window > Trace checkable item.
    pub trace_item: CheckMenuItem<Wry>,
    /// The File > Exit item.
    pub exit_item: MenuItem<Wry>,
}

/// Holds the File > Open Recent submenu so its items can be replaced in
/// place whenever the recent-profiles list changes (see
/// `recent::record_recent_profile`), without rebuilding the whole app menu.
pub struct RecentMenuState(pub Submenu<Wry>);

/// Builds the native app menu (File/Edit/Window/Help) and the menu-item
/// handles needed to keep Window-menu checkboxes in sync with window
/// visibility. Checked state for Terminal/Trace is initialized from each
/// window's current `is_visible()`. The File > Open Recent submenu starts
/// empty — populated once the recent-profiles list is loaded, via
/// `rebuild_open_recent_submenu`.
pub fn build_menu(app: &tauri::App) -> tauri::Result<(Menu<Wry>, WindowMenuState, RecentMenuState)> {
    // A plain `MenuItem` rather than `PredefinedMenuItem::quit`: muda's GTK
    // backend silently drops `Quit` (it isn't in its short list of supported
    // predefined types on Linux), so the item never appeared at all. The
    // click is dispatched to the same `request_exit` the "quit" command
    // (bound to Ctrl+Q — see `App.tsx`) already uses. Both the menu
    // accelerator and the JS-level binding are main-window-only (issue
    // #351): the menu is stripped from the Terminal/Trace windows in
    // `run()`'s `setup`, and Ctrl+Q is handled locally in `App.tsx` rather
    // than via the cross-window `APP_KEY_BINDINGS` array.
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

    let terminal_visible = window_is_visible(app, TERMINAL_WINDOW_LABEL);
    let terminal_item =
        CheckMenuItem::with_id(app, TOGGLE_TERMINAL_ID, "Terminal", true, terminal_visible, None::<&str>)?;
    let trace_visible = window_is_visible(app, TRACE_WINDOW_LABEL);
    let trace_item = CheckMenuItem::with_id(app, TOGGLE_TRACE_ID, "Trace", true, trace_visible, None::<&str>)?;
    let window_menu = Submenu::with_items(app, "Window", true, &[&terminal_item, &trace_item])?;

    let about_item = PredefinedMenuItem::about(app, None, None)?;
    let help_menu = Submenu::with_items(app, "Help", true, &[&about_item])?;

    let menu = Menu::with_items(app, &[&file_menu, &edit_menu, &window_menu, &help_menu])?;

    Ok((menu, WindowMenuState { terminal_item, trace_item, exit_item }, RecentMenuState(open_recent_submenu)))
}

fn window_is_visible(app: &tauri::App, label: &str) -> bool {
    app.get_webview_window(label).and_then(|w| w.is_visible().ok()).unwrap_or(false)
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

/// Toggles the visibility of the window labeled `label` and updates
/// `check_item`'s checked state to match the new visibility.
///
/// Used by the Window-menu item's own click handler and by the
/// `toggle_terminal_visibility`/`toggle_trace_visibility` commands (bound to
/// Ctrl+Shift+`` `/Ctrl+Shift+T), so every path that can show or hide one of
/// these windows keeps the menu checkbox consistent.
pub fn toggle_window_visibility(app: &AppHandle, label: &str, check_item: &CheckMenuItem<Wry>) -> Result<(), String> {
    let window = app.get_webview_window(label).ok_or_else(|| format!("{label} window not found"))?;
    let visible = window.is_visible().map_err(|e| e.to_string())?;
    if visible { window.hide() } else { window.show() }.map_err(|e| e.to_string())?;
    sync_checkbox(check_item, !visible);
    Ok(())
}

/// Sets `check_item`'s checked state to `visible`, best-effort.
///
/// Used wherever a window's visibility changes through a path other than
/// `toggle_window_visibility` — namely the native close button, which always
/// hides rather than toggling.
pub fn sync_checkbox(check_item: &CheckMenuItem<Wry>, visible: bool) {
    let _ = check_item.set_checked(visible);
}
