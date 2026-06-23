# Terminal Window Sizing Plan

## Problem Summary

The terminal window must display exactly 80 columns by 24 rows of monospace text
with no clipping. Achieving this proved difficult because of an interaction
between three layers:

1. **Tauri's logical pixel model.** `WebviewWindowBuilder::inner_size(w, h)`
   specifies dimensions in logical pixels, which are scaled by the system's text
   scaling factor before reaching the webview as CSS pixels. On the development
   host the GNOME text scaling factor is 1.25, so a logical width of 840
   produces only 672 CSS pixels in the webview.

2. **Xterm.js fractional cell widths.** Xterm.js measures the font's advance
   width and multiplies by the column count to determine the canvas size. With
   Ubuntu Mono at 16px the cell width was 8.9625px, making the total grid width
   717.0px. The fractional per-cell width means the 80th column's rightmost
   pixel can be lost to rounding.

3. **Wayland window resize semantics.** Programmatic `set_size` calls after
   window creation are not reliably honored by Wayland compositors, so a
   strategy of creating the window at a default size and then resizing it to
   fit the measured terminal grid does not work.

## Current Solution

The window is created at a hardcoded logical size of 848x572, which at 1.25x
scaling yields approximately 678x458 CSS pixels — enough to contain the
80x24 grid at fontSize 15 (Ubuntu Mono) without clipping. The window is
non-resizable and the FitAddon is not used; the terminal renders at its natural
fixed geometry.

This works but is brittle: the hardcoded size is correct only for this specific
combination of font, font size, and display scale factor.

## Future Approach

The goal is to support user-configurable font and font size while producing a
correctly-sized window on any display scale factor.

### Strategy: measure then create

The terminal window should be sized *after* the font metrics are known but
*before* the window becomes visible. The sequence:

1. Create the terminal window as hidden (`visible: false` in builder).
2. In the frontend, instantiate the `Terminal` with the configured font/size,
   open it into the container, and measure `.xterm-screen`'s bounding rect.
3. Query `window.devicePixelRatio` (which under Tauri/WebKitGTK reflects the
   effective scale factor including text scaling).
4. Compute the required logical size:
   ```
   logicalWidth  = Math.ceil(screenRect.width * devicePixelRatio)
   logicalHeight = Math.ceil(screenRect.height * devicePixelRatio)
   ```
5. Invoke a Tauri command that calls `set_size` on the *still-hidden* window,
   then calls `show()`. Because the window has not yet been mapped by the
   compositor, the size request should be applied as the initial geometry.

### Fallback: scale-aware initial size

If the measure-then-create strategy is unreliable on some compositors, an
alternative is to query the scale factor on the Rust side before creating the
window:

```rust
let scale = monitor.scale_factor(); // from Tauri's monitor API
let logical_w = (css_width_needed * scale).ceil();
```

This requires knowing the CSS pixel dimensions ahead of time, which means either
hardcoding per-font-size tables or measuring once during first run and caching
the result in user preferences.

### Additional considerations

- **Font size preference.** Store the user's chosen font size in the debugger
  config file (`~/.emma/debugger/default/ui.toml` or similar). The terminal
  component reads it at startup.
- **Integer cell widths.** When possible, prefer font sizes that produce integer
  (or half-integer) cell widths to avoid sub-pixel rounding at column 80. For
  Ubuntu Mono, sizes that work well can be discovered empirically and suggested
  to the user.
- **Resize after creation.** If future Wayland/Tauri versions reliably support
  post-map resize, the measure-then-resize approach becomes viable again and is
  simpler than the hidden-window dance.
- **HiDPI vs text scaling.** `devicePixelRatio` conflates hardware DPI scaling
  with GNOME's text scaling factor. If these need to be distinguished, the Rust
  side can read `org.gnome.desktop.interface text-scaling-factor` via GSettings
  or the `GNOME_TEXT_SCALING_FACTOR` env var.
