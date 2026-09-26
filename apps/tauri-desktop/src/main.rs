// Single-window Tauri desktop shell for the dsh web UI. No console window,
// no separate browser tab, no Explorer windows: this process spawns
// `dsh --profile web` in the background and shows its UI directly in one
// native webview window, the same way apps/desktop's Electron shell does.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

/// The wrapped `dsh web` child process, tracked through its lifecycle so a
/// shutdown request racing with the still-in-flight spawn cannot leak it
/// (see [`shutdown`]).
enum ChildSlot {
    /// `spawn_web_profile` has not returned yet.
    Pending,
    /// `dsh web` is running as this child.
    Running(Child),
    /// `shutdown` has already run. A child that finishes spawning after this
    /// (see the `Err` arm in `main`'s spawn thread) must be killed
    /// immediately rather than stored here, since nothing will ever look at
    /// this slot again to stop it.
    Stopped,
}

/// Holds the wrapped `dsh web` child's lifecycle state for the app's
/// lifetime, so window-close can terminate it.
struct WebProfile {
    child: Mutex<ChildSlot>,
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

/// Spawns `dsh --profile web` bound to `host:port`. Stdout and stderr are
/// both piped rather than discarded: `dsh web` announces its authenticated
/// launch URL on stdout (index requests without that URL's token or an
/// existing session cookie are rejected with 401, so a bare
/// `http://host:port` cannot be constructed by this shell and must be read
/// from that announcement instead), and, if the web profile fails to start,
/// writes the actionable cause to stderr — this shell's packaged Windows
/// release has no console to show either stream in otherwise, so both are
/// read back and surfaced in the window instead. Stdin is discarded.
fn spawn_web_profile(host: &str, port: &str) -> std::io::Result<Child> {
    dsh_command()
        .args(["--profile", "web", "--host", host, "--port", port, "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Extracts the authenticated URL from a `dsh web` readiness line
/// (`"dsh web: <url>"`, optionally followed by `" (LAN: <url>)"`), or
/// `None` if `line` is not that announcement.
fn parse_ready_url(line: &str) -> Option<String> {
    let url = line.strip_prefix("dsh web: ")?.split_whitespace().next()?;
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

/// Reads `stderr` on a dedicated thread, returning a handle to its
/// accumulated lines (bounded to the most recent `MAX_LINES`, so a runaway
/// or looping process cannot grow this without bound) for the caller to
/// read back if `dsh web` fails to announce readiness.
fn spawn_stderr_capture(stderr: ChildStderr) -> Arc<Mutex<Vec<String>>> {
    const MAX_LINES: usize = 200;
    let lines = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&lines);
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let mut lines = writer.lock().expect("stderr capture mutex poisoned");
            if lines.len() >= MAX_LINES {
                lines.remove(0);
            }
            lines.push(line);
        }
    });
    lines
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
                let mut guard = profile.child.lock().expect("web profile mutex poisoned");
                if let ChildSlot::Running(child) = &mut *guard {
                    if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
                        return None;
                    }
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

/// Kills and waits on the wrapped `dsh web` child process, if it is running
/// or still spawning. Called on window close and on app exit so no orphaned
/// `dsh` process survives the shell.
///
/// Marks the shared slot `Stopped` before anything else, atomically with
/// respect to the spawn thread's own check in `main`: if `dsh` is still in
/// the process of spawning (the slot is `Pending`) when this runs, this
/// leaves nothing to kill directly, but the spawn thread will see `Stopped`
/// once it finishes and kill that child itself instead of handing it to the
/// window. Without that shared check, a window closed in the brief window
/// between the spawn starting and its `Child` being recorded here would
/// leave `dsh web` running unmanaged after the app had already begun
/// exiting: `shutdown` would find `Pending`/nothing to stop, and the spawn
/// thread would have no way to know a shutdown had already started.
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
    let previous = {
        let mut guard = profile.child.lock().expect("web profile mutex poisoned");
        std::mem::replace(&mut *guard, ChildSlot::Stopped)
    };
    let mut child = match previous {
        ChildSlot::Running(child) => child,
        ChildSlot::Pending | ChildSlot::Stopped => return,
    };

    // wait_for_ready_url's own polling may already have observed this child
    // exit via try_wait() without anyone taking it out of `profile.child`. On
    // Unix, a try_wait() that returns an exit status has already reaped the
    // process, so its numeric PID could since have been recycled by the OS;
    // signaling it now (via `kill`/`taskkill` by PID) could hit an unrelated
    // process rather than this one. If it has already exited, there is
    // nothing left to stop.
    match child.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(_) => return,
    }

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

const TIMEOUT_MESSAGE: &str =
    "dsh web did not become reachable within 30s. Check that `dsh` is on PATH and the configured port is free, then restart.";

/// Percent-encodes the characters that a `data:` URL's opaque path cannot
/// carry literally: `#` and `?` both terminate that path early (starting a
/// fragment or query, per the generic URL syntax, even for a
/// cannot-be-a-base scheme like `data:`), `%` would otherwise be
/// misinterpreted as the start of an existing percent-encoding, and raw
/// control characters (e.g. a newline from a wrapped error message) are
/// invalid there outright. Everything else is passed through unchanged.
fn encode_for_data_url(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            c if (c as u32) < 0x20 => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Builds a `data:` URL for a short plain-text diagnostic message, styled
/// the same as [`LOADING_HTML`].
fn message_page(message: &str) -> String {
    format!(
        "data:text/html,<!doctype html><html><body style='background:%23111;color:%23eee;\
         font-family:sans-serif;padding:2rem;white-space:pre-wrap'><p>{}</p></body></html>",
        encode_for_data_url(message)
    )
}

/// Builds the page shown when `dsh web` never announced readiness: the
/// captured stderr tail if `dsh` wrote one (the CLI's own actionable cause
/// for the web profile failing to start), otherwise the generic timeout
/// message.
fn startup_failure_page(captured_stderr: &Mutex<Vec<String>>) -> String {
    let diagnostic = captured_stderr.lock().expect("stderr capture mutex poisoned").join("\n");
    if diagnostic.trim().is_empty() {
        message_page(TIMEOUT_MESSAGE)
    } else {
        message_page(&format!("dsh web failed to start:\n\n{diagnostic}"))
    }
}

/// Spawns the `dsh web` profile, opens a single window showing a loading
/// placeholder, then navigates that window to the profile's announced,
/// authenticated URL once it is ready (or to a diagnostic page on launch
/// failure or a 30s timeout). Tears the child process down on window close
/// or app exit.
///
/// The window is built before `dsh` is ever spawned, and spawning happens
/// only in the background thread after that succeeds: a `dsh` launch
/// failure (missing PATH entry, bad `DSH_CLI_PATH`) is shown as a message in
/// the already-open window rather than panicking before any window exists —
/// which, in the packaged Windows release (`windows_subsystem = "windows"`),
/// would otherwise fail with no visible error at all. It also means a
/// window-build failure can never happen after a child was already spawned,
/// so `setup`'s only fallible step that runs before the child exists cannot
/// leak it.
fn main() {
    let host = env::var("DSH_WEB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("DSH_WEB_PORT").unwrap_or_else(|_| "5175".to_string());

    tauri::Builder::default()
        .manage(WebProfile { child: Mutex::new(ChildSlot::Pending) })
        .setup(move |app: &mut tauri::App| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(LOADING_HTML.parse()?))
                .title("DeepSeek Harness")
                .inner_size(1280.0, 860.0)
                .min_inner_size(800.0, 600.0)
                .build()?;

            let handle: AppHandle = app.handle().clone();
            thread::spawn(move || {
                let profile = handle.state::<WebProfile>();
                let url = match spawn_web_profile(&host, &port) {
                    Ok(mut child) => {
                        let stdout = child.stdout.take().expect("spawn_web_profile pipes stdout");
                        let stderr = child.stderr.take().expect("spawn_web_profile pipes stderr");
                        let captured_stderr = spawn_stderr_capture(stderr);

                        let mut guard = profile.child.lock().expect("web profile mutex poisoned");
                        let recorded = match std::mem::replace(&mut *guard, ChildSlot::Pending) {
                            ChildSlot::Pending => {
                                *guard = ChildSlot::Running(child);
                                Ok(())
                            }
                            ChildSlot::Stopped => {
                                *guard = ChildSlot::Stopped;
                                Err(child)
                            }
                            ChildSlot::Running(_) => unreachable!("spawn_web_profile runs exactly once"),
                        };
                        drop(guard);

                        match recorded {
                            Ok(()) => wait_for_ready_url(&profile, stdout, Duration::from_secs(30))
                                .unwrap_or_else(|| startup_failure_page(&captured_stderr)),
                            Err(mut child) => {
                                // shutdown() already ran while dsh was still spawning: it
                                // found nothing to stop and returned. Stop this one now
                                // instead of leaving it to run unmanaged after the app has
                                // already begun exiting; there is no window left to show
                                // anything in, so just return.
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                        }
                    }
                    Err(error) => message_page(&format!(
                        "failed to launch `dsh --profile web` (checked DSH_CLI_PATH, then PATH): {error}"
                    )),
                };
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
