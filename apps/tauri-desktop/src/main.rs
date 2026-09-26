// Single-window Tauri desktop shell for the dsh web UI. No console window,
// no separate browser tab, no Explorer windows: this process spawns
// `dsh --profile web` in the background and shows its UI directly in one
// native webview window, the same way apps/desktop's Electron shell does.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

/// The wrapped `dsh web` child process. Held for the app's lifetime so
/// window-close can terminate it.
struct WebProfile {
    child: Mutex<Option<Child>>,
}

/// Adds the Windows `CREATE_NO_WINDOW` flag so a console-subsystem process
/// (`cmd.exe`, an npm shim, `taskkill`) spawned from this GUI-subsystem app
/// does not flash its own console window. Redirecting stdio alone does not
/// prevent that console from being created. A no-op on other platforms.
#[cfg(windows)]
fn no_console_window(mut command: Command) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(not(windows))]
fn no_console_window(command: Command) -> Command {
    command
}

/// Locates the `dsh` launcher: `DSH_CLI_PATH` overrides discovery for
/// packaging or development and is launched directly (it already names a
/// concrete executable, extension included). Otherwise `dsh` is resolved
/// from `PATH`, which is how an end-user install of the harness CLI is
/// expected to be reached. On Windows that install is commonly an npm-style
/// `dsh.cmd` shim, which `Command::new("dsh")` cannot find on its own (Rust
/// does not apply `PATHEXT` the way `cmd.exe` does), so the fallback runs
/// through `cmd /C`, which does.
fn dsh_command() -> Command {
    let command = if let Ok(path) = env::var("DSH_CLI_PATH") {
        Command::new(path)
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", "dsh"]);
        command
    } else {
        Command::new("dsh")
    };
    no_console_window(command)
}

/// Spawns `dsh --profile web` bound to `host:port`. Stdout is piped (not
/// discarded): `dsh web` announces its authenticated launch URL there, and
/// index requests without that URL's token or an existing session cookie
/// are rejected with 401, so a bare `http://host:port` cannot be
/// constructed by this shell and must be read from that announcement
/// instead. Stdin and stderr are discarded, since this shell has no console
/// to show the latter in.
fn spawn_web_profile(host: &str, port: &str) -> std::io::Result<Child> {
    dsh_command()
        .args(["--profile", "web", "--host", host, "--port", port, "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

/// Extracts the authenticated URL from a `dsh web` readiness line
/// (`"dsh web: <url>"`, optionally followed by `" (LAN: <url>)"`), or
/// `None` if `line` is not that announcement.
fn parse_ready_url(line: &str) -> Option<String> {
    let url = line.strip_prefix("dsh web: ")?.split_whitespace().next()?;
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

/// Blocks the calling thread until `dsh web` announces its authenticated
/// launch URL on `stdout`, the child exits without announcing one, or
/// `timeout` elapses. Returns that URL on success.
///
/// Reading stdout for the announcement (rather than polling the configured
/// TCP port and assuming success) is both the only URL this shell can
/// legitimately navigate to — the index route 401s without its token — and
/// a stronger readiness signal than a TCP connect: it is only ever printed
/// by this exact child once its own server has bound, so it cannot be
/// satisfied by an unrelated process already occupying the port.
fn wait_for_ready_url(profile: &WebProfile, stdout: ChildStdout, timeout: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(url) = parse_ready_url(&line) {
                let _ = tx.send(url);
                return;
            }
        }
    });

    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(url) => return Some(url),
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut child = profile.child.lock().expect("web profile mutex poisoned");
                if matches!(child.as_mut().map(|c| c.try_wait()), Some(Ok(Some(_))) | Some(Err(_))) {
                    return None;
                }
            }
        }
    }
}

/// Polls `child` until it exits or `timeout` elapses, without killing it.
/// Used to give a graceful stop request time to work before escalating to a
/// forced kill.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Kills and waits on the wrapped `dsh web` child process, if it is still
/// running. Called on window close and on app exit so no orphaned `dsh`
/// process survives the shell.
///
/// Tries a graceful stop first (`SIGTERM` on Unix, `taskkill` without `/F`
/// on Windows) and gives it 5s to exit before force-killing, so the CLI can
/// run its normal shutdown path (flushing Session/storage state) rather
/// than always being cut off by an uncatchable kill.
///
/// On Windows, `dsh` is launched through `cmd /C` (see `dsh_command`), so
/// the tracked child is `cmd.exe`, not the Node process it starts in turn;
/// killing only that direct child would leak the Node process and its
/// bound port. `taskkill /T` targets the whole process tree instead.
fn shutdown(profile: &State<WebProfile>) {
    let Some(mut child) = profile.child.lock().expect("web profile mutex poisoned").take() else {
        return;
    };

    if cfg!(target_os = "windows") {
        let _ = no_console_window(Command::new("taskkill")).arg("/PID").arg(child.id().to_string()).arg("/T").status();
    } else {
        let _ = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status();
    }

    if wait_for_exit(&mut child, Duration::from_secs(5)).is_none() {
        if cfg!(target_os = "windows") {
            let _ = no_console_window(Command::new("taskkill"))
                .arg("/PID")
                .arg(child.id().to_string())
                .arg("/T")
                .arg("/F")
                .status();
        } else {
            let _ = child.kill();
        }
    }
    let _ = child.wait();
}

const LOADING_HTML: &str = "data:text/html,\
<!doctype html><html><body style='background:%23111;color:%23eee;\
font-family:sans-serif;display:flex;align-items:center;\
justify-content:center;height:100vh;margin:0'>\
<p>Starting DeepSeek Harness&hellip;</p></body></html>";

const TIMEOUT_HTML: &str = "data:text/html,\
<!doctype html><html><body style='background:%23111;color:%23eee;\
font-family:sans-serif;padding:2rem'>\
<p>dsh web did not become reachable within 30s. Check that `dsh` \
is on PATH and the configured port is free, then restart.</p></body></html>";

/// Spawns the `dsh web` profile, opens a single window showing a loading
/// placeholder, then navigates that window to the profile's announced,
/// authenticated URL once it is ready (or to a timeout page after 30s).
/// Tears the child process down on window close or app exit.
fn main() {
    let host = env::var("DSH_WEB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("DSH_WEB_PORT").unwrap_or_else(|_| "5175".to_string());

    let mut child = spawn_web_profile(&host, &port).unwrap_or_else(|error| {
        panic!("failed to launch `dsh --profile web` (checked DSH_CLI_PATH, then PATH): {error}");
    });
    let stdout = child.stdout.take().expect("spawn_web_profile pipes stdout");

    tauri::Builder::default()
        .manage(WebProfile { child: Mutex::new(Some(child)) })
        .setup(move |app: &mut tauri::App| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(LOADING_HTML.parse()?))
                .title("DeepSeek Harness")
                .inner_size(1280.0, 860.0)
                .min_inner_size(800.0, 600.0)
                .build()?;

            let handle: AppHandle = app.handle().clone();
            thread::spawn(move || {
                let profile = handle.state::<WebProfile>();
                let url = wait_for_ready_url(&profile, stdout, Duration::from_secs(30))
                    .unwrap_or_else(|| TIMEOUT_HTML.to_string());
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.navigate(url.parse().expect("well-formed URL"));
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                shutdown(&window.state::<WebProfile>());
            }
        })
        .build(tauri::generate_context!())
        .expect("error building the DSH desktop shell")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                shutdown(&app_handle.state::<WebProfile>());
            }
        });
}
