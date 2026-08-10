// Prevents an additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // GTK's client-side decorations don't repaint their titlebar text after
    // a runtime `window.set_title()` call under GNOME's native Wayland
    // compositor — the call succeeds and the title is set, but the visible
    // decoration never updates. Confirmed the same call renders correctly
    // under XWayland, so force GTK onto X11 here (only if the user hasn't
    // already chosen a backend of their own) rather than leave every
    // runtime title update (profile switching, later window renames) silently
    // broken on native Wayland.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GDK_BACKEND").is_none() {
        // SAFETY: the very first thing main() does, before any other
        // thread exists and before GTK/Tauri initialize.
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    }

    emma65_debugger_lib::run();
}
