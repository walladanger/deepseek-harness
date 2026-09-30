//! Windows desktop carrier for an independently installed application's original Web UI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, Webview, WebviewUrl, Window};

/// Paths in an installed launch file are relative to that installation's root.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Launch {
    schema_version: u32,
    name: String,
    program: String,
    arguments: Vec<String>,
    working_directory: String,
    environment: BTreeMap<String, String>,
    readiness_prefix: String,
    require_token: bool,
    startup_timeout_seconds: u64,
}

/// Resolves owned installation paths without permitting traversal outside the folder.
fn owned_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("Expected a relative installation path: {relative}"));
    }
    Ok(root.join(path))
}

/// Accepts only this backend's authenticated loopback HTTP announcement.
fn ready_url(line: &str, prefix: &str, require_token: bool) -> Option<tauri::Url> {
    let candidate = line.strip_prefix(prefix)?.split_whitespace().next()?;
    let url = tauri::Url::parse(candidate).ok()?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default()? == 0
    {
        return None;
    }
    if require_token {
        let tokens: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "token")
            .collect();
        if url.path() != "/" || tokens.len() != 1 || tokens[0].1.is_empty() {
            return None;
        }
    }
    Some(url)
}

/// Windows Job ownership reaches descendants even if the initial process exits.
#[cfg(windows)]
struct ProcessJob(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
// This exclusively owned Job handle may move between threads; it is never duplicated or accessed concurrently.
unsafe impl Send for ProcessJob {}

#[cfg(windows)]
impl ProcessJob {
    fn assign(child: &Child) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        // These calls own a new handle; configuration and assignment do not borrow it beyond this function.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            );
            if configured == 0 || AssignProcessToJobObject(job, child.as_raw_handle()) == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error);
            }
            Ok(Self(job))
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        // The sole owned handle closes here, terminating processes still in this Job.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Resume the suspended primary thread only after the process belongs to our Job.
#[cfg(windows)]
fn resume_backend(child: &Child) -> std::io::Result<()> {
    use windows_sys::Win32::{
        Foundation::*,
        System::{Diagnostics::ToolHelp::*, Threading::*},
    };
    // A newly spawned suspended process has one thread; every snapshot/thread handle closes before return.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut found = Thread32First(snapshot, &mut entry) != 0;
        while found {
            if entry.th32OwnerProcessID == child.id() {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                CloseHandle(snapshot);
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let result = ResumeThread(thread);
                let error = std::io::Error::last_os_error();
                CloseHandle(thread);
                return if result == u32::MAX {
                    Err(error)
                } else {
                    Ok(())
                };
            }
            found = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Backend primary thread was not found",
        ))
    }
}

struct Backend {
    child: Child,
    #[cfg(windows)]
    job: Option<ProcessJob>,
}

impl Drop for Backend {
    fn drop(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        #[cfg(not(windows))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct ShellState {
    launch: Launch,
    backend: Mutex<Option<Backend>>,
    status: Mutex<String>,
}

#[derive(Serialize)]
struct ShellInfo {
    name: String,
    status: String,
}

/// Only the local chrome WebView can issue native window actions.
#[tauri::command]
fn window_action(webview: Webview, action: String) -> Result<(), String> {
    if webview.label() != "chrome" {
        return Err("Window actions belong to the local shell".into());
    }
    let window = webview.window();
    let result = match action.as_str() {
        "drag" => window.start_dragging(),
        "minimize" => window.minimize(),
        "maximize" => {
            if window.is_maximized().map_err(|e| e.to_string())? {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }
        "close" => window.close(),
        _ => return Err("Unknown window action".into()),
    };
    result.map_err(|e| e.to_string())
}

#[tauri::command]
fn shell_info(webview: Webview, state: tauri::State<ShellState>) -> Result<ShellInfo, String> {
    if webview.label() != "chrome" {
        return Err("Shell information belongs to the local shell".into());
    }
    Ok(ShellInfo {
        name: state.launch.name.clone(),
        status: state.status.lock().map_err(|e| e.to_string())?.clone(),
    })
}

fn set_status(app: &tauri::AppHandle, message: String) {
    if let Ok(mut status) = app.state::<ShellState>().status.lock() {
        *status = message.clone();
    }
    let _ = app.emit_to("chrome", "wrapper-status", message);
}

fn stop(app: &tauri::AppHandle) {
    // Drop waits for the owned backend; holding the lock serializes close and monitoring.
    drop(
        app.state::<ShellState>()
            .backend
            .lock()
            .expect("backend mutex")
            .take(),
    );
}

fn layout(window: &Window) -> tauri::Result<()> {
    let size = window.inner_size()?;
    let height = (40.0 * window.scale_factor()?).round() as u32;
    if let Some(chrome) = window.get_webview("chrome") {
        chrome.set_size(PhysicalSize::new(size.width, height))?;
    }
    if let Some(content) = window.get_webview("content") {
        content.set_position(PhysicalPosition::new(0, height))?;
        content.set_size(PhysicalSize::new(
            size.width,
            size.height.saturating_sub(height),
        ))?;
    }
    Ok(())
}

/// Read bounded records, keeping pipes open after readiness and hiding launch tokens in logs.
fn capture(
    mut reader: impl Read + Send + 'static,
    log: Arc<Mutex<File>>,
    launch: Launch,
    ready: mpsc::Sender<tauri::Url>,
    announce: bool,
) {
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut record = Vec::new();
        let mut oversized = false;
        let mut announced = false;
        while let Ok(count) = reader.read(&mut chunk) {
            if count == 0 {
                break;
            }
            for byte in &chunk[..count] {
                if *byte == b'\n' {
                    if !oversized {
                        let line = String::from_utf8_lossy(&record);
                        let url = if announce {
                            ready_url(
                                line.trim_end_matches('\r'),
                                &launch.readiness_prefix,
                                launch.require_token,
                            )
                        } else {
                            None
                        };
                        let text = if let Some(url) = url {
                            let safe = format!(
                                "{}{} [launch query redacted]",
                                launch.readiness_prefix,
                                url.origin().ascii_serialization()
                            );
                            if !announced {
                                announced = ready.send(url).is_ok();
                            }
                            safe
                        } else {
                            line.into_owned()
                        };
                        if let Ok(mut file) = log.lock() {
                            let _ = writeln!(file, "{text}");
                        }
                    }
                    record.clear();
                    oversized = false;
                } else if !oversized {
                    record.push(*byte);
                    if record.len() > 16384 {
                        record.clear();
                        oversized = true;
                    }
                }
            }
        }
        if !record.is_empty() && !oversized {
            if let Ok(mut file) = log.lock() {
                let _ = writeln!(file, "{}", String::from_utf8_lossy(&record));
            }
        }
    });
}

fn spawn_backend(
    root: &Path,
    launch: &Launch,
    log: Arc<Mutex<File>>,
    ready: mpsc::Sender<tauri::Url>,
) -> Result<Backend, Box<dyn std::error::Error>> {
    let program = owned_path(root, &launch.program)?;
    if matches!(
        program
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd" | "bat" | "ps1")
    ) {
        return Err(
            "The launcher requires a native executable, such as the contained node.exe".into(),
        );
    }
    let mut command = Command::new(program);
    command
        .args(
            launch
                .arguments
                .iter()
                .map(|s| s.replace("{root}", &root.to_string_lossy())),
        )
        .current_dir(owned_path(root, &launch.working_directory)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if ["KEY", "SECRET", "TOKEN", "PASSWORD"]
            .iter()
            .any(|part| upper.contains(part))
        {
            command.env_remove(key);
        }
    }
    for (key, value) in &launch.environment {
        command.env(key, value.replace("{root}", &root.to_string_lossy()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00000004);
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let job = match ProcessJob::assign(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = Command::new("taskkill.exe")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .output();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
    };
    #[cfg(windows)]
    if let Err(error) = resume_backend(&child) {
        drop(job);
        let _ = child.wait();
        return Err(error.into());
    }
    capture(
        child.stdout.take().expect("piped stdout"),
        Arc::clone(&log),
        launch.clone(),
        ready.clone(),
        true,
    );
    capture(
        child.stderr.take().expect("piped stderr"),
        log,
        launch.clone(),
        ready,
        false,
    );
    Ok(Backend {
        child,
        #[cfg(windows)]
        job: Some(job),
    })
}

/// Exercises the real backend and authenticated HTTP entry without opening a window.
fn smoke(
    root: &Path,
    launch: &Launch,
    log: Arc<Mutex<File>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx) = mpsc::channel();
    let mut backend = spawn_backend(root, launch, Arc::clone(&log), tx)?;
    let deadline = Instant::now() + Duration::from_secs(launch.startup_timeout_seconds);
    let url = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(url) => break url,
            Err(_) => {
                if let Some(status) = backend.child.try_wait()? {
                    return Err(format!("Backend exited before readiness: {status}").into());
                }
                if Instant::now() >= deadline {
                    return Err("Backend startup timed out".into());
                }
            }
        }
    };
    let target = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    let response = http_get(&url, &target, None)?;
    let line = response.lines().next().unwrap_or("");
    let page = if line.starts_with("HTTP/1.1 200") {
        response.clone()
    } else if ["HTTP/1.1 302", "HTTP/1.1 303", "HTTP/1.1 307"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
    {
        let cookie = response
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("set-cookie")
                    .then(|| value.trim().split(';').next().unwrap_or(""))
            })
            .ok_or("Authenticated redirect did not supply a cookie")?;
        http_get(&url, "/", Some(cookie))?
    } else {
        return Err(format!("Web UI did not accept its launch URL: {line}").into());
    };
    if !page.starts_with("HTTP/1.1 200") || !page.to_ascii_lowercase().contains("<html") {
        return Err("The authenticated Web UI did not return an HTML page".into());
    }
    writeln!(
        log.lock().map_err(|e| e.to_string())?,
        "wrapper smoke: accepted authenticated Web UI ({line})"
    )?;
    drop(backend);
    Ok(())
}

fn http_get(
    url: &tauri::Url,
    target: &str,
    cookie: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut socket = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from((
            [127, 0, 0, 1],
            url.port_or_known_default().ok_or("Missing port")?,
        )),
        Duration::from_secs(10),
    )?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    let authentication = cookie
        .map(|value| format!("Cookie: {value}\r\n"))
        .unwrap_or_default();
    write!(
        socket,
        "GET {target} HTTP/1.1\r\nHost: {}\r\n{authentication}Connection: close\r\n\r\n",
        url.authority()
    )?;
    let mut response = String::new();
    socket.take(128 * 1024).read_to_string(&mut response)?;
    Ok(response)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = std::env::args_os()
        .nth(1)
        .ok_or("Pass the installation's launch.json path")?;
    let config_path = dunce::canonicalize(config_path)?;
    let root = config_path
        .parent()
        .ok_or("Missing installation root")?
        .to_path_buf();
    let root = dunce::simplified(&root).to_path_buf();
    let launch: Launch = serde_json::from_slice(&fs::read(&config_path)?)?;
    if launch.schema_version != 1
        || launch.readiness_prefix.is_empty()
        || launch.startup_timeout_seconds == 0
    {
        return Err("Unsupported or incomplete launch.json".into());
    }
    fs::create_dir_all(root.join("data/logs"))?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let log_path = root.join(format!("data/logs/backend-{stamp}.log"));
    let log = Arc::new(Mutex::new(
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log_path)?,
    ));
    if std::env::args()
        .skip(2)
        .any(|argument| argument == "--smoke")
    {
        let result = smoke(&root, &launch, Arc::clone(&log));
        if let Err(error) = &result {
            let _ = writeln!(
                log.lock().expect("log mutex"),
                "wrapper smoke failed: {error}"
            );
        }
        return result;
    }
    let origin = Arc::new(Mutex::new(None::<String>));
    let build_launch = launch.clone();
    let mut installation_hash = std::hash::DefaultHasher::new();
    root.hash(&mut installation_hash);
    let mut context = tauri::generate_context!();
    context.config_mut().identifier = format!(
        "com.walladanger.portable-web-shell.i{:x}",
        installation_hash.finish()
    );
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .manage(ShellState {
            launch,
            backend: Mutex::new(None),
            status: Mutex::new("Starting…".into()),
        })
        .invoke_handler(tauri::generate_handler![window_action, shell_info])
        .setup(move |app| {
            let window = tauri::window::WindowBuilder::new(app, "main")
                .title(&build_launch.name)
                .decorations(false)
                .inner_size(1400.0, 900.0)
                .min_inner_size(800.0, 600.0)
                .build()?;
            window.add_child(
                tauri::webview::WebviewBuilder::new("chrome", WebviewUrl::App("index.html".into()))
                    .data_directory(root.join("data/webview-chrome")),
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(1400, 40),
            )?;
            let navigation_origin = Arc::clone(&origin);
            let content = tauri::webview::WebviewBuilder::new(
                "content",
                WebviewUrl::App("loading.html".into()),
            )
            .data_directory(root.join("data/webview-app"))
            .on_navigation(move |url| {
                if url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost") {
                    return true;
                }
                let allowed = navigation_origin.lock().expect("navigation mutex");
                if allowed.as_deref() == Some(url.origin().ascii_serialization().as_str()) {
                    true
                } else {
                    open_external(url);
                    false
                }
            })
            .on_new_window(|url, _| {
                open_external(&url);
                tauri::webview::NewWindowResponse::Deny
            });
            window.add_child(
                content,
                PhysicalPosition::new(0, 40),
                PhysicalSize::new(1400, 860),
            )?;
            layout(&window)?;
            let (tx, rx) = mpsc::channel();
            match spawn_backend(&root, &build_launch, log, tx) {
                Ok(backend) => {
                    *app.state::<ShellState>()
                        .backend
                        .lock()
                        .expect("backend mutex") = Some(backend)
                }
                Err(error) => {
                    set_status(
                        app.handle(),
                        format!("Error: {error}. See {}", log_path.display()),
                    );
                    return Ok(());
                }
            }
            let handle = app.handle().clone();
            thread::spawn(move || {
                let deadline =
                    Instant::now() + Duration::from_secs(build_launch.startup_timeout_seconds);
                let mut loaded = false;
                loop {
                    if !loaded {
                        if let Ok(url) = rx.recv_timeout(Duration::from_millis(100)) {
                            *origin.lock().expect("navigation mutex") =
                                Some(url.origin().ascii_serialization());
                            if let Some(view) = handle.get_webview("content") {
                                if let Err(error) = view.navigate(url) {
                                    set_status(&handle, format!("Error: {error}"));
                                    stop(&handle);
                                    return;
                                }
                            } else {
                                stop(&handle);
                                return;
                            }
                            loaded = true;
                            set_status(&handle, "Ready".into());
                        }
                    } else {
                        thread::sleep(Duration::from_millis(200));
                    }
                    let state = handle.state::<ShellState>();
                    let outcome = {
                        let mut backend = state.backend.lock().expect("backend mutex");
                        match backend.as_mut() {
                            None => return,
                            Some(backend) => backend.child.try_wait(),
                        }
                    };
                    match outcome {
                        Ok(Some(status)) => {
                            set_status(
                                &handle,
                                format!(
                                    "Error: application exited ({status}). See {}",
                                    log_path.display()
                                ),
                            );
                            stop(&handle);
                            return;
                        }
                        Err(error) => {
                            set_status(&handle, format!("Error: {error}"));
                            stop(&handle);
                            return;
                        }
                        Ok(None) => {}
                    }
                    if !loaded && Instant::now() >= deadline {
                        set_status(
                            &handle,
                            format!("Error: startup timed out. See {}", log_path.display()),
                        );
                        stop(&handle);
                        return;
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => {
                let _ = layout(window);
            }
            tauri::WindowEvent::CloseRequested { .. } => {
                stop(window.app_handle());
                window.app_handle().exit(0);
            }
            _ => {}
        })
        .build(context)?
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                stop(app);
            }
        });
    Ok(())
}

/// Open citations and authorization pages through Windows' URL handler without cmd.exe.
fn open_external(url: &tauri::Url) {
    if !matches!(url.scheme(), "http" | "https") {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url.as_str()])
            .creation_flags(0x08000000)
            .spawn();
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Desktop launcher: {error}");
        #[cfg(windows)]
        if !std::env::args().any(|arg| arg == "--smoke") {
            let message: Vec<u16> = format!(
                "{error}\n\nRun Install.cmd or inspect this installation's data/logs folder."
            )
            .encode_utf16()
            .chain(Some(0))
            .collect();
            let title: Vec<u16> = "Desktop launcher could not start"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            // Both strings remain alive for this synchronous native dialog.
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    title.as_ptr(),
                    0x10,
                );
            }
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_requires_loopback_and_one_token() {
        let parse = |s| ready_url(s, "dsh web: ", true);
        assert!(parse("dsh web: http://127.0.0.1:1234/?token=abc\r").is_some());
        for invalid in [
            "dsh web: http://127.0.0.1:1234@evil.example/?token=x",
            "dsh web: http://127.0.0.1:1234/",
            "dsh web: http://127.0.0.1:1234/?token=a&token=b",
            "dsh web: http://127.0.0.1:1234/path?token=a",
        ] {
            assert!(parse(invalid).is_none(), "{invalid}");
        }
    }
    #[test]
    fn owned_paths_reject_absolute_paths_and_parent_traversal() {
        let root = Path::new("C:/app");
        assert!(owned_path(root, "runtime/node/node.exe").is_ok());
        for invalid in ["../outside", "C:/outside", "/outside", ""] {
            assert!(owned_path(root, invalid).is_err());
        }
    }
}
