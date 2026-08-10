//! Native application menu bar: File/Edit/Window/Help.

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, Wry};

use crate::terminal::TERMINAL_WINDOW_LABEL;
use crate::trace::TRACE_WINDOW_LABEL;

/// Menu item id for the Window > Terminal checkable item.
pub(crate) const TOGGLE_TERMINAL_ID: &str = "toggle-terminal";
/// Menu item id for the Window > Trace checkable item.
pub(crate) const TOGGLE_TRACE_ID: &str = "toggle-trace";
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

/// Builds the native app menu (File/Edit/Window/Help) and the menu-item
/// handles needed to keep Window-menu checkboxes in sync with window
/// visibility. Checked state for Terminal/Trace is initialized from each
/// window's current `is_visible()`.
pub fn build_menu(app: &tauri::App) -> tauri::Result<(Menu<Wry>, WindowMenuState)> {
    // A plain `MenuItem` rather than `PredefinedMenuItem::quit`: muda's GTK
    // backend silently drops `Quit` (it isn't in its short list of supported
    // predefined types on Linux), so the item never appeared at all. The
    // click is dispatched to the same `app.exit(0)` the "quit" command
    // (bound to Ctrl+Q — see `useAppKeyBindings.ts`) already uses.
    let exit_item = MenuItem::with_id(app, EXIT_ID, "Exit", true, None::<&str>)?;
    let file_menu = Submenu::with_items(app, "File", true, &[&exit_item])?;

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

    Ok((menu, WindowMenuState { terminal_item, trace_item, exit_item }))
}

fn window_is_visible(app: &tauri::App, label: &str) -> bool {
    app.get_webview_window(label).and_then(|w| w.is_visible().ok()).unwrap_or(false)
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
