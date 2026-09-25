// Single-window Tauri desktop shell for the dsh web UI. No console window,
// no separate browser tab, no Explorer windows: this process spawns
// `dsh --profile web` in the background and shows its UI directly in one
// native webview window, the same way apps/desktop's Electron shell does.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

/// Host/port the wrapped `dsh web` profile listens on, and the child process
/// once spawned. Held for the app's lifetime so window-close can terminate it.
struct WebProfile {
    host: String,
    port: String,
    child: Mutex<Option<Child>>,
}

impl WebProfile {
    /// The loopback URL the wrapped `dsh web` profile serves on, built from
    /// its configured host and port.
    fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
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
    if let Ok(path) = env::var("DSH_CLI_PATH") {
        return Command::new(path);
    }
    if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", "dsh"]);
        command
    } else {
        Command::new("dsh")
    }
}

/// Spawns `dsh --profile web` bound to `host:port` with stdio discarded,
/// since this shell has no console to show it in.
fn spawn_web_profile(host: &str, port: &str) -> std::io::Result<Child> {
    dsh_command()
        .args(["--profile", "web", "--host", host, "--port", port, "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Kills and waits on the wrapped `dsh web` child process, if it is still
/// running. Called on window close and on app exit so no orphaned `dsh`
/// process survives the shell.
///
/// On Windows, `dsh` is launched through `cmd /C` (see `dsh_command`), so
/// the tracked child is `cmd.exe`, not the Node process it starts in turn;
/// `Child::kill` would only stop `cmd.exe` and leak that Node process and
/// its bound port. `taskkill /T` kills the whole process tree instead.
fn shutdown(profile: &State<WebProfile>) {
    if let Some(mut child) = profile.child.lock().expect("web profile mutex poisoned").take() {
        if cfg!(target_os = "windows") {
            let _ = Command::new("taskkill")
                .arg("/PID")
                .arg(child.id().to_string())
                .arg("/T")
                .arg("/F")
                .status();
        } else {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

/// Blocks the calling thread until the wrapped `dsh web` child accepts a TCP
/// connection on its configured host/port, the child exits early, or
/// `timeout` elapses. Used to hold the loading screen until `dsh web` is
/// actually ready, instead of navigating the window straight into a
/// connection-refused error page.
///
/// Checking the child's own status on each poll (rather than treating any
/// successful TCP connect as readiness) avoids two failure modes: waiting
/// out the full timeout against a child that already crashed, and — if an
/// unrelated process happens to already own the configured port — reporting
/// readiness for a server that was never actually started here.
fn wait_for_port(profile: &WebProfile, timeout: Duration) -> bool {
    let address = format!("{}:{}", profile.host, profile.port);
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        {
            let mut child = profile.child.lock().expect("web profile mutex poisoned");
            match child.as_mut().map(|c| c.try_wait()) {
                Some(Ok(Some(_status))) => return false,
                Some(Ok(None)) | None => {}
                Some(Err(_)) => return false,
            }
        }
        if TcpStream::connect(&address).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(150));
    }
    false
}

const LOADING_HTML: &str = "data:text/html,\
<!doctype html><html><body style='background:#111;color:#eee;\
font-family:sans-serif;display:flex;align-items:center;\
justify-content:center;height:100vh;margin:0'>\
<p>Starting DeepSeek Harness&hellip;</p></body></html>";

/// Spawns the `dsh web` profile, opens a single window showing a loading
/// placeholder, then navigates that window to the profile once it accepts
/// connections (or to an error page after a 30s timeout). Tears the child
/// process down on window close or app exit.
fn main() {
    let host = env::var("DSH_WEB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("DSH_WEB_PORT").unwrap_or_else(|_| "5175".to_string());

    let child = spawn_web_profile(&host, &port).unwrap_or_else(|error| {
        panic!("failed to launch `dsh --profile web` (checked DSH_CLI_PATH, then PATH): {error}");
    });

    tauri::Builder::default()
        .manage(WebProfile { host, port, child: Mutex::new(Some(child)) })
        .setup(|app: &mut tauri::App| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(LOADING_HTML.parse()?))
                .title("DeepSeek Harness")
                .inner_size(1280.0, 860.0)
                .min_inner_size(800.0, 600.0)
                .build()?;

            let handle: AppHandle = app.handle().clone();
            thread::spawn(move || {
                let profile = handle.state::<WebProfile>();
                let ready = wait_for_port(&profile, Duration::from_secs(30));
                let url = if ready {
                    profile.url()
                } else {
                    "data:text/html,<p style='font-family:sans-serif;padding:2rem'>\
                     dsh web did not become reachable within 30s. Check that `dsh` \
                     is on PATH and the port is free, then restart.</p>"
                        .to_string()
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
