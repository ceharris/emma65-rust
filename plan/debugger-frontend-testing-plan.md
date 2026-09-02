# Debugger Frontend Testing (Issue #482)

## Context

`debugger/frontend` is a Vite 5 + React 18 + TypeScript 5 project with zero test
infrastructure today: no test runner, no `@testing-library/*`, and no `test` script in
`package.json`. The only build-time check is `tsc && vite build` (the `build` script),
and per this repo's `CLAUDE.md` that hook only runs under `cargo tauri dev`/
`cargo tauri build` — plain `cargo build`, which is what `.github/workflows/ci.yml`'s
`ci-debugger` job actually runs (`cargo build -p emma65-debugger`), does not invoke it.
There is no Node/npm step anywhere in `ci.yml` today, so nothing in the frontend is
exercised by CI at all right now — not type-checked, not built, not tested. Fixing that
gap (Work Unit 1) is as important as adding the tests themselves.

The `src/` tree is 33 files, ~8,300 lines: components/contexts in `.tsx` files and a
handful of plain `.ts` modules. 18 of the 33 files import `invoke`/`listen`/
`getCurrentWindow`/`emitTo` from `@tauri-apps/api` directly, so most components are
coupled to the Tauri IPC bridge, not just to React state. Two modules —
`terminalPreferences.ts` (183 lines) and most of `RadixControl.tsx`'s pure exports — have
no Tauri or DOM dependency at all and are the cheapest, highest-value place to start.
`terminalSizing.ts` is *mostly* pure but its `isMonospaceFont`/`measureCell` helpers
touch `<canvas>`/a live `xterm` `Terminal` instance, and `logicalSizeForCssPixels` calls
`getCurrentWindow()` — verified directly against the current source, this file is a mix
of unit-testable and infra-needing code, not uniformly pure. Component size varies
widely, from small presentational pieces (`NumberStepper.tsx`, `RadixControl.tsx`,
`SelectPopover.tsx`, `ColorPickerPopover.tsx`, ~60-100 lines) up to large, stateful
panels (`MemoryPanel.tsx` 1179 lines, `AssemblerPanel.tsx` 680, `TerminalPanel.tsx` 539,
`TracePanel.tsx` 476, `DisassemblyPanel.tsx` 449, `RegisterPanel.tsx` 398,
`RunControlsContext.tsx` 406).

### Framework: Vitest, not Jest

**Vitest** + **`@testing-library/react`** + **`@testing-library/jest-dom`** +
**`@testing-library/user-event`**, with **`jsdom`** as the test environment:

- The project is already on Vite. Vitest reuses `vite.config.ts` (same esbuild/TS
  transform, same path resolution, same SCSS handling) with a small `test:` block added
  — no parallel Babel/`ts-jest` transform pipeline to configure and keep in sync.
- Native ESM and `import.meta` support without extra shims; the project's
  `tsconfig.json` already targets `"module": "ESNext"` / `"moduleResolution":
  "bundler"`, which Jest needs extra config (`ts-jest`, `moduleNameMapper`,
  `transformIgnorePatterns`) to approximate.
- Jest-compatible API (`describe`/`it`/`expect`, `vi.fn()`/`vi.mock()` near-drop-in for
  `jest.fn()`/`jest.mock()`), so `@testing-library/react` and
  `@testing-library/jest-dom` work unmodified.
- Faster in watch mode and CI (no separate transform step, worker-based isolation),
  which matters given `ci-debugger` already runs in a container job per PR.

### Scope and what's realistically testable

- **High value, low cost — pure logic.** `terminalPreferences.ts` in full (no Tauri/DOM
  imports at all — verified: its only import is `type { ITheme } from "@xterm/xterm"`);
  `RadixControl.tsx`'s pure exports (`formatDataRadix`, `useRadixCycle`/
  `useDataRadix` via `renderHook`, the `*_RADIX_CYCLE` constants); the pure half of
  `terminalSizing.ts` (`TERMINAL_SIZE_PRESETS`, `pixelSizeForGrid`, and
  `isMonospaceFont` once its `<canvas>` call is mocked — jsdom has no real canvas
  backend, so this needs a `getContext`/`measureText` stub, not a from-scratch DOM).
  Start here — no Tauri mock needed for `terminalPreferences.ts` or the `RadixControl`
  pieces.
- **Needs the Tauri mock, but still pure-shaped.** `terminalSizing.ts`'s
  `logicalSizeForCssPixels` (calls `getCurrentWindow()`) and `measureCell` (needs a real
  or heavily-stubbed `xterm` `Terminal`, low value — likely skip); `useAppKeyBindings.ts`
  — `APP_KEY_BINDINGS[].matches` is pure and testable with synthetic `KeyboardEvent`s,
  but `.run()` and the `useAppKeyBindings()` hook itself call `invoke`/`emitTo`/
  `getCurrentWindow` and need the Unit 1 Tauri mock plus `renderHook`.
- **Small presentational components.** `NumberStepper`, `SelectPopover`,
  `ColorPickerPopover`, `RadixButton` (from `RadixControl.tsx`) are good
  `@testing-library/react` render/interaction targets and, confirmed by checking their
  imports, don't touch `@tauri-apps/api` at all — no mock needed.
- **`StatusBar.tsx`'s formatting helpers.** `splitSpeed`/`formatCycles` are pure but
  currently module-private (not exported) — either export them for direct unit testing
  or cover them indirectly by rendering `StatusBar` (which pulls in
  `ExecutionContext`/`RunControlsContext` and therefore the Tauri mock); prefer
  exporting, it's a one-line change and keeps the test independent of context wiring.
- **Contexts.** `ExecutionContext`, `RunControlsContext`, `EditMenuContext`,
  `ThemeContext` all call `invoke`/`listen` directly (confirmed) and need a shared test
  helper that mocks `@tauri-apps/api/core` (`invoke`) and `@tauri-apps/api/event`
  (`listen`) — built once in Unit 1, reused everywhere after. Test via `renderHook`
  against each context's `use*` hook wrapped in its provider, or a thin test consumer
  component, rather than mounting the full app tree. `RunControlsContext.tsx` also
  exports pure helpers (`sliderToInterval`/`intervalToSlider`/`SLIDER_STEPS`) that can be
  unit tested directly alongside the context tests, no mock needed for those specific
  exports.
- **Low value or high cost — skip or defer.** `TerminalPanel.tsx` wraps `@xterm/xterm`,
  and `DisplayPanel.tsx`/`LedMatrixPanel.tsx` render to `<canvas>`; none of these render
  meaningfully in `jsdom` and would need heavy mocking to test at all. `MemoryPanel.tsx`
  and `AssemblerPanel.tsx` are large and stateful enough that full render-level coverage
  is expensive relative to what it buys. The better long-term move for these —
  consistent with how `terminalSizing.ts`/`terminalPreferences.ts` were already split out
  of `TerminalPanel.tsx` — is to keep extracting pure logic out of the big panels into
  plain `.ts` modules that unit tests can cover directly, rather than chasing
  full-component render coverage. Not in scope for this plan.
- **CI enforcement.** Add a `test` script (`vitest run`) and a Node/npm setup step to the
  existing `ci-debugger` job (gated the same way, via `needs.changes.outputs.debugger`)
  so regressions actually fail PRs. Since there's currently no Node in that job at all
  (confirmed: no `npm`/`node` reference anywhere in `.github/workflows/ci.yml`, and the
  CI Docker image at `.github/docker/ci/Dockerfile` installs no Node toolchain), this
  needs `actions/setup-node` added to the job rather than assuming Node is already
  present — see Work Unit 1 for the concrete choice.

## Work Units

Per this project's usual workflow: one branch/PR per unit, stop after each and await
explicit instruction before merging or starting the next.

### 1. Test infrastructure

- Add devDependencies: `vitest`, `@testing-library/react`, `@testing-library/jest-dom`,
  `@testing-library/user-event`, `jsdom`. Pin versions compatible with Vite 5 / React 18
  / the installed TypeScript 5.
- Add a `test:` block to `vite.config.ts` (environment `jsdom`, `globals: true` so
  `describe`/`it`/`expect` don't need per-file imports, a `setupFiles` entry pointing at
  a new `src/test/setup.ts` that imports `@testing-library/jest-dom`'s matchers). Keep it
  in the existing `vite.config.ts` rather than a separate `vitest.config.ts` — this
  project has one Vite config today and there's no stated reason to fork it; revisit
  only if the `test:` block ever needs settings that conflict with the build config.
- Add the `test` npm script (`"test": "vitest run"`, plus optionally `"test:watch":
  "vitest"` for local dev).
- Build the shared Tauri mock helper, e.g. `src/test/tauriMock.ts`:
  - `vi.mock("@tauri-apps/api/core", ...)` exposing a mockable `invoke` (default: a
    `vi.fn()` a test can `mockResolvedValueOnce`/`mockImplementation` per call).
  - `vi.mock("@tauri-apps/api/event", ...)` exposing a mockable `listen` that returns an
    unlisten function, plus a small fake event bus (`emitMockEvent(name, payload)`) so a
    test can simulate a backend-pushed event (e.g. `breakpoints-changed`,
    `debugger-running-tick`) reaching a registered listener.
  - `vi.mock("@tauri-apps/api/window", ...)` exposing a mockable `getCurrentWindow()`
    (default label `"main"`, overridable per test for the detached-window branches in
    `useAppKeyBindings.ts`).
  - `vi.mock("@tauri-apps/api/event", ...)`'s `emitTo` mock, needed by
    `useAppKeyBindings.ts`'s `revealPanel`.
  - Document the helper's usage pattern (call `resetTauriMocks()` in `beforeEach`, or
    similar) since every later unit depends on it.
- Add one or two smoke tests for `terminalPreferences.ts` (pure, no mock needed) to
  prove the pipeline end to end before building out the mock helper's own tests.
- CI: add a Node setup + `npm ci` + `npm test` sequence to the `ci-debugger` job in
  `.github/workflows/ci.yml`, gated on `needs.changes.outputs.debugger == 'true'` like
  the rest of that job. Two ways to get Node into the container-based job — pick one and
  note the choice in the PR:
  - Add an `actions/setup-node@v4` step (with `node-version` pinned and
    `cache: npm`/`cache-dependency-path: debugger/frontend/package-lock.json`) ahead of
    the existing `Build`/`Clippy` steps, working directly in the
    `ghcr.io/.../-ci:latest` container — simplest, no image rebuild needed.
  - Or bake a Node toolchain into `.github/docker/ci/Dockerfile` and rebuild/republish
    the image via `.github/workflows/docker-ci-image.yml` first — only worth it if a
    later unit needs more than `npm ci && npm test` (e.g. if the frontend build itself
    gets added to CI too), otherwise it's unnecessary image churn for this plan.
  - Recommend the `actions/setup-node` route for this plan: it's scoped to exactly what
    Work Unit 1 needs (installing test deps and running Vitest), doesn't touch the
    shared CI image other jobs also use, and is the minimal fix for the "nothing in the
    frontend is checked by CI" gap. Revisit baking Node into the image if `npm run
    build`/`tsc` type-checking is ever added to CI as a separate follow-up (out of
    scope here — see note below).
  - Add the step(s) as a `Frontend test` (and, working-directory `debugger/frontend`)
    step within the existing `ci-debugger` job rather than a new top-level job — it's
    already gated on the same `debugger/**` path filter and there's no reason to
    duplicate that gating in a sibling job.
- Out of scope for this plan, flag as a follow-up in the PR description: wiring `tsc`/
  `vite build` itself into CI. That's a real gap (per the Context section above, `cargo
  build -p emma65-debugger` doesn't currently build or type-check the frontend at all in
  CI), but it's a separate concern from adding *tests* — raise it as a candidate for a
  quick follow-up PR once this plan's CI step exists and the Node setup is already in
  place, rather than conflating it with Unit 1's scope.

### 2. Pure logic/util coverage

- `terminalPreferences.ts` — full coverage: `themeWithTextOverrides`,
  `themeWithCursorOverrides`, `terminalKeyActionBytes` (all three action variants: `bs`
  → `[0x08]`, `del` → `[0x7f]`, `dch` → the literal ANSI DCH byte sequence), and the
  `ANSI_PALETTE_FIELDS`/`ANSI_PRESET_COLORS` constant shapes if there's any derived
  logic worth asserting (e.g. that every palette field has a `themeKey` matching
  `ITheme`'s field names).
- `terminalSizing.ts`'s pure/mockable exports: `TERMINAL_SIZE_PRESETS` (shape/values),
  `pixelSizeForGrid` (given a fake/minimal `Terminal`-shaped object with the metrics it
  reads), `isMonospaceFont` (mock `HTMLCanvasElement.prototype.getContext` to return a
  stub `measureText` returning fixed widths for the probe strings, then assert the
  monospace/non-monospace branches). Skip `measureCell` (needs real xterm internals) and
  treat `logicalSizeForCssPixels` (needs the Unit 1 `getCurrentWindow` mock) as a
  stretch item for this unit rather than a requirement.
- `RadixControl.tsx`'s pure exports: `formatDataRadix` (all five `DataRadix` variants:
  `hex`/`udec`/`sdec`/`oct`/`bin`, including negative-number handling for `sdec`),
  `useRadixCycle`/`useDataRadix` via `renderHook` (cycling through
  `DATA_RADIX_CYCLE`/`ADDR_RADIX_CYCLE`/`STACK_RADIX_CYCLE` and back to start).
- `useAppKeyBindings.ts`'s `APP_KEY_BINDINGS[].matches` predicates: construct synthetic
  `KeyboardEvent`s (`ctrlKey`/`shiftKey`/`code` combinations) and assert each binding
  matches only its intended combo (Ctrl+Shift+T/D/M) and rejects near-misses (missing
  modifier, wrong `code`). This part needs no Tauri mock.
- Using the Unit 1 mock, also cover `useAppKeyBindings()` the hook itself: mount it via
  `renderHook`, dispatch a matching `keydown` on `window`, and assert the right `invoke`/
  `emitTo` call happened for both the main-window branch (binding with
  `hasMainWindowAccelerator: true` should be skipped) and a detached-window branch
  (`getCurrentWindow` mocked to return e.g. `terminal-detached`, asserting
  `invoke("attach_terminal")` fires instead of `emitTo`).
- Export `splitSpeed`/`formatCycles` from `StatusBar.tsx` (currently private) and unit
  test them directly.

### 3. Small presentational components

Using `@testing-library/react` + `@testing-library/user-event`, no Tauri mock needed
for any of these (confirmed: none import `@tauri-apps/api`):

- `NumberStepper` — renders the current value, +/- buttons increment/decrement within
  `min`/`max`, typing a value and blurring/Enter commits via `onChange`, clamps
  out-of-range input.
- `SelectPopover` — opens on click, renders `options`, selecting an option calls
  `onChange` and closes the popover, closes on outside click/Escape.
- `ColorPickerPopover` — opens on click, renders the `ANSI_PRESET_COLORS` swatches,
  clicking a swatch calls `onChange` with its hex value, the "Custom…" path (native
  `<input type="color">`) at least renders and is present (jsdom doesn't simulate a real
  color-picker UI, so assert wiring, not the OS dialog).
- `RadixButton` (from `RadixControl.tsx`) — renders the current radix's label, clicking
  calls `onCycle`, `stopPropagation` behavior when set.

### 4. Contexts

Using the Unit 1 Tauri mock, test each context's `use*` hook via `renderHook` wrapped in
its own provider (or a minimal test consumer component where a hook alone doesn't
exercise enough):

- `ThemeContext` — `resolveTheme(mode, prefersDark)` pure-function cases first (no mock
  needed: `"auto"` follows `prefersDark`, `"dark"`/`"light"` are fixed); then
  `ThemeProvider`/`useTheme` — initial theme resolution, `setThemeMode` calls
  `invoke("set_theme", { mode })`.
- `EditMenuContext` — `EditMenuProvider`/`useEditMenuOverride` — setting flags calls
  `invoke("set_edit_menu_enabled", { flags })` with the expected shape; default (no
  provider) returns `null` from `useEditMenuOverride`.
- `ExecutionContext` — `ExecutionProvider`/`useExecutionContext` — registers a
  `debugger-running-tick` listener on mount via the Unit 1 fake event bus, unregisters on
  unmount; state updates when a tick event fires.
- `RunControlsContext` — the largest of the four (406 lines): cover the `invoke` calls
  for `step_over`/`step_return`/`run_cpu`/`stop_cpu`, the `debugger-cpu-reset` listener
  registration/cleanup, and `set_run_controls_enabled`/`set_profile_menu_enabled`/
  `set_recent_menu_enabled` firing with the right derived flags for a couple of
  representative `ExecState` transitions (e.g. stopped → running → stopped). Don't chase
  full coverage of every state transition in this unit — enough to prove the pattern and
  catch regressions in the commands that matter most (run/stop).

### 5. Selected mid-size panels (stretch — revisit after 1-4 land)

Confirmed IPC surface for the four candidates named in the issue's outline:
`BreakpointPanel.tsx` (`get_breakpoints`, `set_breakpoint`, `enable_breakpoint`/
`disable_breakpoint`, `remove_breakpoint`, `resolve_symbol`, `breakpoints-changed`
listener), `WatchpointPanel.tsx`, `SymbolsPanel.tsx`, `RegisterPanel.tsx`
(`get_registers`, `set_register`) — all mockable with the Unit 1 helper, all have a
fairly clear input (mocked `invoke` responses) → output (rendered rows, form state)
contract. This unit is explicitly a stretch goal: only start it after 1-4 are merged and
have proven the mock/test patterns hold up in practice; the exact panel(s) attempted and
how much of each is worth covering is a call to make at that point, not fixed in advance
here.

`TerminalPanel`, `DisplayPanel`, `LedMatrixPanel`, `MemoryPanel`, and `AssemblerPanel`
are deliberately excluded from this entire plan for the reasons in the Context section
above (canvas/xterm rendering `jsdom` can't do meaningfully, or size/complexity that
outweighs the payoff) — worth a separate discussion once the pure-logic-extraction
pattern from Unit 2 has proven itself on a couple of the smaller panels.

## Key files

- `debugger/frontend/package.json` — new devDependencies, `test` script
- `debugger/frontend/vite.config.ts` — new `test:` block
- `debugger/frontend/src/test/setup.ts` (new) — jest-dom matcher setup
- `debugger/frontend/src/test/tauriMock.ts` (new) — shared `invoke`/`listen`/
  `getCurrentWindow`/`emitTo` mocks, reused from Unit 1 onward
- `debugger/frontend/src/terminalPreferences.ts` — Unit 2 target, plus its new
  `*.test.ts` file
- `debugger/frontend/src/terminalSizing.ts` — Unit 2 target (partial), plus its new
  `*.test.ts` file
- `debugger/frontend/src/RadixControl.tsx` — Unit 2 (pure exports) and Unit 3
  (`RadixButton`) target, plus its new `*.test.ts(x)` file(s)
- `debugger/frontend/src/useAppKeyBindings.ts` — Unit 2 target, plus its new
  `*.test.ts` file
- `debugger/frontend/src/StatusBar.tsx` — Unit 2 (export `splitSpeed`/`formatCycles`)
- `debugger/frontend/src/NumberStepper.tsx`, `SelectPopover.tsx`,
  `ColorPickerPopover.tsx` — Unit 3 targets, plus new `*.test.tsx` files
- `debugger/frontend/src/ThemeContext.tsx`, `ExecutionContext.tsx`,
  `EditMenuContext.tsx`, `RunControlsContext.tsx` — Unit 4 targets, plus new
  `*.test.tsx` files
- `debugger/frontend/src/BreakpointPanel.tsx`, `WatchpointPanel.tsx`,
  `SymbolsPanel.tsx`, `RegisterPanel.tsx` — Unit 5 stretch targets
- `.github/workflows/ci.yml` — `ci-debugger` job: Node setup + `npm ci` + `npm test`
  steps

## Verification

- `npm test` (`vitest run`) clean after every unit, run from `debugger/frontend`.
- `npm run build` (`tsc && vite build`) still clean after every unit — new test files
  must type-check too (`tsconfig.json`'s `"include": ["src"]` picks them up
  automatically; confirm `vitest`'s and `@testing-library/*`'s ambient types don't
  conflict with existing `strict`/`noUnusedLocals`/`noUnusedParameters` settings, adding
  a `tsconfig` `types` entry or a separate `tsconfig.test.json` only if that surfaces a
  real conflict).
- `cargo build --workspace`, `cargo clippy --workspace --all-targets` clean after Unit 1
  (CI workflow YAML change plus, if the debugger crate is touched at all — it shouldn't
  need to be for this plan).
- After Unit 1: push a throwaway commit or open the PR and confirm the new CI step
  actually runs and passes on GitHub Actions (not just locally) — the point of this unit
  is closing a CI gap, so confirm the gap is actually closed before calling it done.
- For each unit, run `npm test` locally and read the output rather than just checking
  exit code — Vitest's default reporter shows skipped/todo tests distinctly from passing
  ones, worth a quick scan to make sure nothing silently no-ops.

## Workflow

This plan is implemented as five units of work (numbered above), Unit 5 explicitly a
stretch goal to revisit after 1-4 are done. For each: create a branch named for the
unit, do the work plus the validation described above, commit, push to origin, and open
a PR calling out any manual UAT needed. Await explicit instruction before merging that
PR or starting the next unit.
