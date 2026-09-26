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
neither replaces the other yet.

## Launch policy

The shell never launches the harness itself outside a `dsh` profile: at
startup it spawns `dsh --profile web --host <host> --port <port> --no-open`
(overridable via `DSH_CLI_PATH`, `DSH_WEB_HOST`, `DSH_WEB_PORT`) and holds
that child process for its own lifetime. The harness process itself is
always started through the `web` profile, per
[`docs/architecture.md#application-launch`](../../docs/architecture.md#application-launch).

The window shows a short "Starting…" placeholder while it waits (up to 30s)
for `dsh web` to announce its authenticated launch URL on stdout, then
navigates to that exact URL. A bare `http://<host>:<port>` is not usable
here: the index route requires the process token or session cookie carried
in that announced URL, and rejects a plain request with 401 (see
[`dsh-host-frontend-static`](../../packages/host/frontend-static/README.md)).

On window close or app exit, the shell asks the child to stop gracefully
(`SIGTERM` on Unix, `taskkill` without `/F` on Windows) and gives it 5s to
exit — so the CLI can run its normal shutdown path — before force-killing
it. On Windows the child is `cmd.exe` (see below), so termination targets
its whole process tree (`taskkill /T`), not just that one process.

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
  under `%LOCALAPPDATA%\DeepSeek Harness\` automatically.

## Development

Requires the Rust toolchain and, on Windows, the Tauri v2 prerequisites
(WebView2, MSVC build tools). `icons/` holds the bundled window/installer
icons. `tauri.conf.json` declares an empty `app.windows` array because the
single window is created at runtime in `src/main.rs`, before the wrapped
`dsh web` process has announced its authenticated launch URL.
