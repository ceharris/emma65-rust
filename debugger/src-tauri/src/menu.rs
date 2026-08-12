//! Native application menu bar: File/Edit/Window/Help.

use std::path::PathBuf;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Wry};

/// Menu item id for the Window > Terminal item.
pub(crate) const TOGGLE_TERMINAL_ID: &str = "toggle-terminal";
/// Menu item id for the Window > Trace item.
pub(crate) const TOGGLE_TRACE_ID: &str = "toggle-trace";
/// Menu item id for the Window > Log item.
pub(crate) const TOGGLE_LOG_ID: &str = "toggle-log";
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

/// Holds the File > Exit item so `on_menu_event` can match its id against
/// the clicked item. Terminal, Trace, and Log (since #384/#383) are all
/// plain, non-checkable "Reveal…" items — they don't reflect any
/// window-visibility boolean, so their click is matched by id alone in
/// `on_menu_event` and no handle to them needs to be kept here.
pub struct WindowMenuState {
    /// The File > Exit item.
    pub exit_item: MenuItem<Wry>,
}

/// Holds the File > Open Recent submenu so its items can be replaced in
/// place whenever the recent-profiles list changes (see
/// `recent::record_recent_profile`), without rebuilding the whole app menu.
pub struct RecentMenuState(pub Submenu<Wry>);

/// Builds the native app menu (File/Edit/Window/Help) and the `exit_item`
/// handle `on_menu_event` needs to dispatch an Exit click. The File > Open
/// Recent submenu starts empty — populated once the recent-profiles list is
/// loaded, via `rebuild_open_recent_submenu`.
pub fn build_menu(app: &tauri::App) -> tauri::Result<(Menu<Wry>, WindowMenuState, RecentMenuState)> {
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

    // These get real native accelerators (issue #377), same as the File-menu items
    // above, so the shortcut text renders with native styling instead of being baked
    // into the label. The accelerator only ever fires while the main window (the only
    // one carrying this menu) has focus; `on_menu_event` in `lib.rs` handles the click.
    // These same combos also work via the JS-level listener in `useAppKeyBindings.ts`
    // (needed today for a Terminal/Trace/Log tab not currently focused, and for any
    // future window that installs that hook) — that listener skips them specifically
    // for the main window, so the native accelerator and the JS invoke don't both fire
    // there.
    //
    // Terminal is Ctrl+Shift+T rather than the VS Code-style Ctrl+Shift+` originally
    // wanted (issue #377): on GTK, Shift is a *consumed* modifier for punctuation keys
    // like backtick or digits — it's what selects the shifted symbol (backtick+Shift is
    // "~", 1+Shift is "!") — so registering the accelerator as the unshifted symbol plus
    // a Shift requirement never matches the actual key-press GTK reports, which arrives
    // as the already-shifted character with Shift folded in. Tauri's menu API only
    // accepts a physical-key accelerator string and always builds it from the unshifted
    // symbol, so there's no accelerator string that works around this for punctuation
    // keys. Letters don't have the problem, since GTK canonicalizes a shifted letter to
    // its uppercase keyval, which is exactly how muda registers it — hence Trace/Log
    // below, and Terminal's fallback to a letter. See
    // https://docs.gtk.org/gtk3/class.AccelGroup.html on consumed modifiers.
    //
    // All three are plain items, not checkable: since #383/#384, Terminal/Trace/Log
    // are dockview panels rather than their own window, so there's no visibility
    // boolean to reflect — clicking any of them just reveals its dock tab (see
    // `on_menu_event` in `lib.rs`).
    let terminal_item = MenuItem::with_id(app, TOGGLE_TERMINAL_ID, "Reveal Terminal", true, Some("Ctrl+Shift+T"))?;
    let trace_item = MenuItem::with_id(app, TOGGLE_TRACE_ID, "Reveal Trace", true, Some("Ctrl+Shift+Y"))?;
    let log_item = MenuItem::with_id(app, TOGGLE_LOG_ID, "Reveal Log", true, Some("Ctrl+Shift+L"))?;
    let window_menu = Submenu::with_items(app, "Window", true, &[&terminal_item, &trace_item, &log_item])?;

    let about_item = PredefinedMenuItem::about(app, None, None)?;
    let help_menu = Submenu::with_items(app, "Help", true, &[&about_item])?;

    let menu = Menu::with_items(app, &[&file_menu, &edit_menu, &window_menu, &help_menu])?;

    Ok((menu, WindowMenuState { exit_item }, RecentMenuState(open_recent_submenu)))
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
