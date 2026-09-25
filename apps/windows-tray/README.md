# apps/windows-tray

English

## Summary

Experimental windowless Windows wrapper, built with Tauri and Rust, for
running the `dsh web` profile in the background. It creates no Tauri window
and no console: only a system tray icon with **Open DSH Web** (opens the
default browser to the running profile) and **Quit** (stops the wrapped
`dsh` process and exits). This package is Rust/Tauri, not a `packages/`
TypeScript workspace, and is excluded from the pnpm workspace and the `dsh`
launch surface it wraps.

It is being evaluated alongside the existing Electron-based `apps/desktop`;
neither replaces the other yet.

## Launch policy

The wrapper never launches the harness itself outside a `dsh` profile: at
startup it spawns `dsh --profile web --host <host> --port <port> --no-open`
(overridable via `DSH_CLI_PATH`, `DSH_WEB_HOST`, `DSH_WEB_PORT`) and holds
that child process for its own lifetime, killing it on Quit or exit. The
harness process itself is always started through the `web` profile, per
[`docs/architecture.md#application-launch`](../../docs/architecture.md#application-launch).

## Use

- Development (any platform with `dsh` on `PATH`): `cargo run` from this
  directory.
- Windows, without installing: build a release binary
  (`cargo build --release`) and run it via
  [`scripts/Run-DshWebTray.ps1`](scripts/Run-DshWebTray.ps1), which starts it
  fully hidden (no console, no taskbar entry) and can stop it with `-Stop`.
- Windows, installed: the
  [Windows Tray Installer workflow](../../.github/workflows/windows-tray-installer.yml)
  produces an MSI and an NSIS `.exe` installer via `tauri-action`; after
  installing, `scripts/Run-DshWebTray.ps1` finds the installed executable
  under `%LOCALAPPDATA%\DSH Web Tray\` automatically.

## Development

Requires the Rust toolchain and, on Windows, the Tauri v2 prerequisites
(WebView2, MSVC build tools). `icons/` holds the bundled tray/installer
icons. `tauri.conf.json` declares an empty `app.windows` array, so no
webview window is ever created.
