# apps/tauri-desktop

English

## Summary

Experimental lightweight desktop shell, built with Tauri and Rust, for the
`dsh web` UI. It opens one normal native window (title bar, minimize,
maximize, close — no custom chrome) and loads the running `dsh web` server
into it directly; there is no separate browser tab, no console window, and
no extra windows. This package is Rust/Tauri, not a `packages/` TypeScript
workspace, and is excluded from the pnpm workspace and the `dsh` launch
surface it wraps.

It is being evaluated alongside the existing Electron-based `apps/desktop`;
neither replaces the other yet. Its product name is deliberately distinct
("DeepSeek Harness (Tauri Preview)") so installing it alongside `apps/desktop`
does not collide with — or overwrite — that app's Start-menu shortcut and
installer identity, both of which use the plain "DeepSeek Harness" name.

## Launch policy

The shell never launches the harness itself outside a `dsh` profile: at
startup it spawns `dsh --profile web --host 127.0.0.1 --port <port> --no-open`
(overridable via `DSH_CLI_PATH`, `DSH_WEB_PORT`) in a working directory
(overridable via `DSH_WEB_WORKSPACE`, otherwise the current user's home
directory) and holds that child process for its own lifetime. `127.0.0.1`
is not configurable: `dsh web`'s own configuration schema accepts only that
or `0.0.0.0`, and the CLI itself refuses `0.0.0.0` for safety, so it is not
a real setting. The harness process itself is always started through the
`web` profile, per
[`docs/architecture.md#application-launch`](../../docs/architecture.md#application-launch).

The window shows a short "Starting…" placeholder while it waits (up to 30s)
for `dsh web` to announce its authenticated launch URL on stdout, then
navigates to that exact URL. A bare `http://127.0.0.1:<port>` is not usable
here: the index route requires the process token or session cookie carried
in that announced URL, and rejects a plain request with 401 (see
[`dsh-host-frontend-static`](../../packages/host/frontend-static/README.md)).
If `dsh web` exits without ever announcing that URL, the window instead
shows whatever it wrote to stderr (its own actionable cause), falling back
to a generic timeout message if it wrote nothing.

On window close or app exit, the shell stops the child. On Unix that's a
graceful `SIGTERM` first, with 5s to exit — so the CLI can run its normal
shutdown path — before escalating to `SIGKILL`. On Windows there is no
graceful step: `dsh` and its `cmd.exe` wrapper are console processes, and
`taskkill` without `/F` does not terminate those (a fact this repository's
own `packages/subprocess/subprocess-local` records and works around with
Windows console-signal machinery this shell does not reimplement), so it
goes straight to a forced, whole-tree `taskkill /T /F`.

A shutdown request that arrives while `dsh` is still in the process of
spawning, or while an earlier shutdown/timeout path is still in the middle
of stopping it, blocks until that resolves rather than finding nothing yet
to do and returning while the child could still end up running unmanaged.
That wait is deliberately unbounded: an unresponsive `DSH_CLI_PATH` (a stuck
UNC path, say) could in principle block the app from closing for as long as
the underlying `Command::spawn()` call takes to fail, rather than for any
fixed bound.

On Unix, a `SIGINT` (Ctrl-C in a foreground `cargo run` session) or
`SIGTERM` delivered to this process directly runs the same stop rather than
letting the OS's default disposition end the process with no chance to
clean up: neither signal reaches Tauri's own window-close handling, and
[moving `dsh` out of this process's own process group](#launch-policy) means
`dsh` would not receive a terminal's Ctrl-C either, so without this handler
it would keep running, bound to its port, after the shell itself was gone.

## Known limitations

This is an early evaluation shell, and two gaps are accepted for now rather
than fixed:

- No live monitoring of the running `dsh web` process: if it crashes after
  the window has already loaded it, the window is simply left showing a
  disconnected page, with no restart or failure UI.
- No workspace picker: `DSH_WEB_WORKSPACE` (or the home-directory default)
  is fixed for the process's lifetime; there is no in-app way to choose or
  change it.

## Use

- Development (any platform with `dsh` on `PATH`): `cargo run` from this
  directory.
- Windows, without installing: build a release binary
  (`cargo build --release`) and run it via
  [`scripts/Run-DshDesktop.ps1`](scripts/Run-DshDesktop.ps1), or the built
  executable directly.
- Windows, installed: the
  [Tauri Desktop Installer workflow](../../.github/workflows/tauri-desktop-installer.yml)
  produces an MSI and an NSIS `.exe` installer via `tauri-action`; after
  installing, `scripts/Run-DshDesktop.ps1` finds the installed executable
  under `%LOCALAPPDATA%\DeepSeek Harness (Tauri Preview)\` automatically.

## Development

Requires the Rust toolchain and, on Windows, the Tauri v2 prerequisites
(WebView2, MSVC build tools). `icons/` holds the bundled window/installer
icons. `tauri.conf.json` declares an empty `app.windows` array because the
single window is created at runtime in `src/main.rs`, before the wrapped
`dsh web` process has announced its authenticated launch URL.
