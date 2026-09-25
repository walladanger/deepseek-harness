// Windowless Windows tray wrapper: keeps `dsh --profile web` running in the
// background and exposes only a tray icon (Open / Quit). No Tauri window is
// created, so the process never shows a taskbar entry or a webview window;
// the dsh web profile is reached through a normal OS browser tab instead.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State};

/// Host/port the wrapped `dsh web` profile listens on, and the child process
/// once spawned. Held for the app's lifetime so Quit can terminate it.
struct WebProfile {
    host: String,
    port: String,
    child: Mutex<Option<Child>>,
}

impl WebProfile {
    fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

/// Locates the `dsh` launcher: `DSH_CLI_PATH` overrides discovery for
/// packaging or development; otherwise `dsh` is resolved from `PATH`, which
/// is how an end-user install of the harness CLI is expected to be reached.
fn dsh_command() -> Command {
    match env::var("DSH_CLI_PATH") {
        Ok(path) => Command::new(path),
        Err(_) => Command::new("dsh"),
    }
}

fn spawn_web_profile(host: &str, port: &str) -> std::io::Result<Child> {
    dsh_command()
        .args(["--profile", "web", "--host", host, "--port", port, "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(target_os = "windows"))]
    let _ = Command::new("xdg-open").arg(url).spawn();
}

fn shutdown(profile: &State<WebProfile>) {
    if let Some(mut child) = profile.child.lock().expect("web profile mutex poisoned").take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn main() {
    let host = env::var("DSH_WEB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("DSH_WEB_PORT").unwrap_or_else(|_| "5175".to_string());

    let child = spawn_web_profile(&host, &port).unwrap_or_else(|error| {
        panic!("failed to launch `dsh --profile web` (checked DSH_CLI_PATH, then PATH): {error}");
    });

    tauri::Builder::default()
        .manage(WebProfile { host, port, child: Mutex::new(Some(child)) })
        .setup(|app: &mut tauri::App| {
            let open_item = MenuItem::with_id(app, "open", "Open DSH Web", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().expect("bundled tray icon"))
                .menu(&menu)
                .tooltip("DSH Web (background)")
                .on_menu_event(|app: &AppHandle, event| {
                    let profile = app.state::<WebProfile>();
                    match event.id().as_ref() {
                        "open" => open_in_browser(&profile.url()),
                        "quit" => {
                            shutdown(&profile);
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|_window, _event| {})
        .build(tauri::generate_context!())
        .expect("error building the DSH web tray wrapper")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                shutdown(&app_handle.state::<WebProfile>());
            }
        });
}
