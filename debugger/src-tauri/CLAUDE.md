# CLAUDE.md — `emma65-debugger`

A Tauri 2 desktop app (`emma65-debugger`) that loads config from
`~/.emma/debugger/profiles/default/emulator.toml`, builds an `EmulatorSession` with an
injected `InternalPipeTransport` wired to its terminal window, and exposes the emulator to a
React/TypeScript frontend (`debugger/frontend/`) via `#[tauri::command]`s. UI preferences
(theme, exit-confirmation skip) are not profile-scoped and live in
`~/.emma/debugger/config/ui.toml` instead. One module per UI panel:

- **`registers`** — register snapshot/edit
- **`cpu_bus`** — reset, IRQ assert/release, NMI trigger, cached bus-signal snapshot
- **`disassembly`** — run/stop/step-into/step-over/step-return, breakpoint CRUD, disassembly listing
- **`memory`** — paged reads/writes/fills/file loads
- **`stack`** — stack pointer and stack page snapshot
- **`terminal`** — console byte-stream bridge and window visibility (toggleable window)
- **`trace`** — live-recorded execution trace, windowed reads
- **`watchpoints`** — loads/compiles `watchpoints.emw`, evaluates on demand, add/remove/edit/toggle with persistence
- **`theme`** — light/dark theme preference; also owns `UiConfig`/`ui.toml` persistence used by
  the exit-confirmation "Don't ask again" preference (set from `lib.rs`'s `confirm_exit`)
- **`menu`** — native File/Edit/Window/Help menu bar and Window-menu checkbox sync
- **`recent`** — recently-used profile list (`~/.emma/debugger/config/recent.toml`), recorded on every
  profile activation and shown in the File > Open Recent submenu
- **`profile`** — `--profile` CLI flag, profile directory resolution, `ensure_profile_dir` (seeds a
  new `default` profile from the bundled `emulator::config::default` template; seeds any other new
  profile by copying files from `default`), New/Open Profile commands, window-title sync

Devices requiring a byte-stream peer (VIA, MC6840, ACIAs) still use their configured
`Transport` independent of the debugger UI; only the console is special-cased to route
through the debugger's own terminal window.
