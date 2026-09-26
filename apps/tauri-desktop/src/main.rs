// Single-window Tauri desktop shell for the dsh web UI. No console window,
// no separate browser tab, no Explorer windows: this process spawns
// `dsh --profile web` in the background and shows its UI directly in one
// native webview window, the same way apps/desktop's Electron shell does.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

/// `dsh web`'s `WebServer.Config` schema (`packages/host/webserver`) accepts
/// only `127.0.0.1` or `0.0.0.0` as `host`, and the Web CLI itself refuses
/// `0.0.0.0` outright ("intentionally not supported yet for safety"). There
/// is therefore exactly one value that can ever work; it is not a real
/// configuration point, so this shell does not expose a host override.
const WEB_HOST: &str = "127.0.0.1";

/// The wrapped `dsh web` child process, tracked through its lifecycle so a
/// shutdown request racing with the still-in-flight spawn cannot leak it
/// (see [`shutdown`]).
enum ChildSlot {
    /// `spawn_web_profile` has not returned yet.
    Pending,
    /// `dsh web` is running as this child.
    Running(Child),
    /// Terminal: either `shutdown` has already run, or `spawn_web_profile`
    /// itself failed (there was never a child to run).
    Stopped,
}

/// Holds the wrapped `dsh web` child's lifecycle state for the app's
/// lifetime, so window-close can terminate it. `child_ready` is signaled
/// whenever `child` leaves [`ChildSlot::Pending`], so `shutdown` can block
/// until the in-flight spawn resolves instead of returning while it is
/// still unknown whether a child will exist to stop.
struct WebProfile {
    child: Mutex<ChildSlot>,
    child_ready: Condvar,
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

/// Resolves the working directory `dsh web` should run in: `DSH_WEB_WORKSPACE`
/// if set, otherwise the user's home directory (`USERPROFILE` on Windows,
/// `HOME` elsewhere), falling back to inheriting this process's own cwd only
/// if neither is set.
///
/// dsh's base-backed profiles (including `web`) treat their invoking
/// directory as the default workspace root. Left unset, a packaged
/// executable launched from a Start-menu shortcut or Explorer inherits
/// whatever working directory that launch happened to use — typically the
/// install directory — which is never where a user wants their session's
/// files read from or written to.
fn workspace_dir() -> Option<String> {
    if let Ok(dir) = env::var("DSH_WEB_WORKSPACE") {
        return Some(dir);
    }
    let home_var = if cfg!(target_os = "windows") { "USERPROFILE" } else { "HOME" };
    env::var(home_var).ok()
}

/// Spawns `dsh --profile web` bound to [`WEB_HOST`]:`port`, in
/// [`workspace_dir`]. Stdout and stderr are both piped rather than
/// discarded: `dsh web` announces its authenticated launch URL on stdout
/// (index requests without that URL's token or an existing session cookie
/// are rejected with 401, so a bare `http://host:port` cannot be
/// constructed by this shell and must be read from that announcement
/// instead), and, if the web profile fails to start, writes the actionable
/// cause to stderr — this shell's packaged Windows release has no console
/// to show either stream in otherwise, so both are read back and surfaced
/// in the window instead. Stdin is discarded.
fn spawn_web_profile(port: &str) -> std::io::Result<Child> {
    let mut command = dsh_command();
    command
        .args(["--profile", "web", "--host", WEB_HOST, "--port", port, "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = workspace_dir() {
        command.current_dir(dir);
    }
    command.spawn()
}

/// Extracts the authenticated URL from a `dsh web` readiness line
/// (`"dsh web: <url>"`, optionally followed by `" (LAN: <url>)"`), or
/// `None` if `line` is not that announcement.
fn parse_ready_url(line: &str) -> Option<String> {
    let url = line.strip_prefix("dsh web: ")?.split_whitespace().next()?;
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

/// Reads `stderr` on a dedicated thread into a bounded buffer, returning a
/// handle to it plus a channel that receives one message once that thread
/// has drained the pipe to EOF (i.e. the process closed it, normally by
/// exiting). The caller reads back from this bounded buffer if `dsh web`
/// fails to announce readiness.
///
/// The buffer is bounded by total bytes retained, not by line count: a
/// `BufRead::lines()`-based reader would still allocate an entire line
/// before any line-count cap applied, so a single very long line (a runaway
/// or looping process without newlines) could grow without bound despite
/// such a cap. Reading fixed-size chunks and keeping only the most recent
/// `MAX_BYTES` of them avoids that.
fn spawn_stderr_capture(stderr: ChildStderr) -> (Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>) {
    const MAX_BYTES: usize = 16 * 1024;
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&buffer);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut chunk = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut reader, &mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut buffer = writer.lock().expect("stderr capture mutex poisoned");
                    buffer.extend_from_slice(&chunk[..n]);
                    if buffer.len() > MAX_BYTES {
                        let excess = buffer.len() - MAX_BYTES;
                        buffer.drain(..excess);
                    }
                }
            }
        }
        let _ = done_tx.send(());
    });
    (buffer, done_rx)
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

/// Stops `child` and waits for it to exit: a graceful request first
/// (`SIGTERM` on Unix, `taskkill` without `/F` on Windows), giving it 5s to
/// exit — so the CLI can run its normal shutdown path (flushing
/// Session/storage state) — before escalating to a forced, whole-tree kill.
///
/// Checks `try_wait()` before signaling at all: on Unix, an exit status
/// already observed by [`wait_for_ready_url`]'s polling means the process
/// was already reaped, so its numeric PID could since have been recycled by
/// the OS, and signaling it now could hit an unrelated process.
///
/// On Windows, `dsh` is launched through `cmd /C` (see `dsh_command`), so
/// `child` is `cmd.exe`, not the Node process it starts in turn; killing
/// only that direct child would leak the Node process and its bound port.
/// `taskkill /T` targets the whole process tree instead, both here and in
/// the escalation.
fn terminate(mut child: Child) {
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

/// Records the outcome of `spawn_web_profile` (`Some(child)` on success,
/// `None` if it failed to spawn at all) into `profile.child`, unless the
/// slot is no longer `Pending` — which only happens if `shutdown`'s bounded
/// wait (see its doc comment) timed out before this ran. In that rare case,
/// any spawned child is `terminate`d here instead, since `shutdown` has
/// already returned and nothing else will ever look at this slot again.
/// Either way, wakes any thread waiting on `child_ready`.
///
/// Returns whether the child (if any) is now the slot's responsibility to
/// stop later (`true`), as opposed to already terminated by this call
/// (`false`) — the caller should not go on to wait for its readiness URL in
/// the latter case, since the app is already tearing down.
fn record_spawn_outcome(profile: &WebProfile, child: Option<Child>) -> bool {
    let mut guard = profile.child.lock().expect("web profile mutex poisoned");
    let accepted = matches!(*guard, ChildSlot::Pending);
    let leftover = if accepted {
        *guard = match child {
            Some(child) => ChildSlot::Running(child),
            None => ChildSlot::Stopped,
        };
        None
    } else {
        child
    };
    drop(guard);
    profile.child_ready.notify_all();
    if let Some(child) = leftover {
        terminate(child);
    }
    accepted
}

/// Stops the wrapped `dsh web` process, if one is running, was still
/// spawning, or never got the chance to (in which case there is nothing to
/// do beyond marking the slot terminal). Called on window close and on app
/// exit so no orphaned `dsh` process survives the shell.
///
/// If the spawn is still in flight (`Pending`), blocks on `child_ready`
/// until [`record_spawn_outcome`] resolves it, rather than returning
/// immediately: `main`'s spawn thread runs detached, so if this returned
/// early while `Pending`, the whole app process could exit — ending that
/// detached thread along with it — before it ever reached the point of
/// storing or killing the child it was in the middle of creating, leaking
/// `dsh web` with no owner left to stop it. The wait is bounded (35s, a
/// margin over the 30s readiness timeout) purely as a safety valve against
/// an unexpected hang; on that timeout this proceeds as if `Stopped`, and
/// `record_spawn_outcome` independently handles a child that finishes
/// spawning after that point.
fn shutdown(profile: &State<WebProfile>) {
    let mut guard = profile.child.lock().expect("web profile mutex poisoned");
    while matches!(*guard, ChildSlot::Pending) {
        let (next, result) = profile
            .child_ready
            .wait_timeout(guard, Duration::from_secs(35))
            .expect("web profile mutex poisoned");
        guard = next;
        if result.timed_out() {
            break;
        }
    }
    let previous = std::mem::replace(&mut *guard, ChildSlot::Stopped);
    drop(guard);
    if let ChildSlot::Running(child) = previous {
        terminate(child);
    }
}

const LOADING_HTML: &str = "data:text/html,\
<!doctype html><html><body style='background:%23111;color:%23eee;\
font-family:sans-serif;display:flex;align-items:center;\
justify-content:center;height:100vh;margin:0'>\
<p>Starting DeepSeek Harness&hellip;</p></body></html>";

const TIMEOUT_MESSAGE: &str =
    "dsh web did not become reachable within 30s. Check that `dsh` is on PATH and the configured port is free, then restart.";

/// Escapes `message` for safe embedding as HTML text content, then
/// percent-encodes the result for a `data:` URL.
///
/// Both steps are necessary and address different boundaries: `message` can
/// contain arbitrary process output (a launch error or `dsh`'s stderr), so
/// without HTML-escaping first, text like `<img onerror=...>` would be
/// parsed as markup by the webview instead of displayed as the literal
/// diagnostic text; percent-encoding alone (as done for the two static
/// pages below, which contain no untrusted text) only protects the URL
/// syntax, not the HTML it decodes to.
fn encode_message(message: &str) -> String {
    let mut escaped = String::with_capacity(message.len());
    for ch in message.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }

    let mut out = String::with_capacity(escaped.len());
    for ch in escaped.chars() {
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

/// Builds a `data:` URL for a short diagnostic message, styled the same as
/// [`LOADING_HTML`]. `message` is treated as untrusted plain text (it may
/// carry arbitrary process output) and rendered as such; see
/// [`encode_message`].
fn message_page(message: &str) -> String {
    format!(
        "data:text/html,<!doctype html><html><body style='background:%23111;color:%23eee;\
         font-family:sans-serif;padding:2rem;white-space:pre-wrap'><p>{}</p></body></html>",
        encode_message(message)
    )
}

/// Builds the page shown when `dsh web` never announced readiness: the
/// captured stderr tail if `dsh` wrote one (the CLI's own actionable cause
/// for the web profile failing to start), otherwise the generic timeout
/// message.
///
/// Waits briefly on `stderr_done` first: `wait_for_ready_url` can observe
/// the child's stdout close (signaling exit) before the separate stderr
/// reader has finished draining its own, otherwise-unrelated pipe, which
/// could otherwise show the generic message despite `dsh` having written an
/// actionable one moments later. The wait is short and always bounded
/// (rather than only applied when the child is confirmed to have exited)
/// because the alternative — blocking until `stderr_done` fires — would
/// hang here indefinitely in the plain timeout case, where the child (and
/// so its stderr) is still running.
fn startup_failure_page(captured_stderr: &Mutex<Vec<u8>>, stderr_done: &mpsc::Receiver<()>) -> String {
    let _ = stderr_done.recv_timeout(Duration::from_millis(500));
    let captured = captured_stderr.lock().expect("stderr capture mutex poisoned");
    let diagnostic = String::from_utf8_lossy(&captured);
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
    let port = env::var("DSH_WEB_PORT").unwrap_or_else(|_| "5175".to_string());

    tauri::Builder::default()
        .manage(WebProfile { child: Mutex::new(ChildSlot::Pending), child_ready: Condvar::new() })
        .setup(move |app: &mut tauri::App| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(LOADING_HTML.parse()?))
                .title("DeepSeek Harness")
                .inner_size(1280.0, 860.0)
                .min_inner_size(800.0, 600.0)
                .build()?;

            let handle: AppHandle = app.handle().clone();
            thread::spawn(move || {
                let profile = handle.state::<WebProfile>();
                let url = match spawn_web_profile(&port) {
                    Ok(mut child) => {
                        let stdout = child.stdout.take().expect("spawn_web_profile pipes stdout");
                        let stderr = child.stderr.take().expect("spawn_web_profile pipes stderr");
                        let (captured_stderr, stderr_done) = spawn_stderr_capture(stderr);

                        if !record_spawn_outcome(&profile, Some(child)) {
                            // shutdown()'s bounded wait timed out before this
                            // resolved and it has already returned; the child
                            // was terminated inside record_spawn_outcome, and
                            // there is nothing left to navigate to.
                            return;
                        }
                        wait_for_ready_url(&profile, stdout, Duration::from_secs(30))
                            .unwrap_or_else(|| startup_failure_page(&captured_stderr, &stderr_done))
                    }
                    Err(error) => {
                        record_spawn_outcome(&profile, None);
                        message_page(&format!(
                            "failed to launch `dsh --profile web` (checked DSH_CLI_PATH, then PATH): {error}"
                        ))
                    }
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
