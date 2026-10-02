//! User-visible window copy, kept in one place so it is not scattered as
//! inline string literals across the window-content code in `main.rs`.

/// English copy for every user-visible string this shell's window can show.
/// There is a single locale for now; this module exists so a future
/// additional locale is a second `Copy` instance here, not a hunt through
/// `main.rs` for embedded literals.
pub struct Copy {
    /// Shown on the loading placeholder page while `dsh web` starts.
    pub starting: &'static str,
    /// Shown when `dsh web` never announced readiness and wrote nothing to
    /// stderr.
    pub timeout_generic: &'static str,
    /// Prefix before `dsh`'s captured stderr when it exited without
    /// announcing readiness.
    pub startup_failed_prefix: &'static str,
    /// Prefix before the OS error when spawning `dsh --profile web` itself
    /// fails.
    pub launch_failed_prefix: &'static str,
    /// Prefix before the parse error when `dsh web` announces a URL that
    /// does not parse.
    pub unparseable_url_prefix: &'static str,
}

pub const EN: Copy = Copy {
    // HTML entity, not a literal ellipsis: `starting` is spliced directly
    // into LOADING_HTML's markup, unescaped (it is a fixed, trusted string,
    // unlike the process output that passes through `encode_message`).
    starting: "Starting DeepSeek Harness&hellip;",
    timeout_generic: "dsh web did not become reachable within 30s. Check that `dsh` is on PATH and the configured port is free, then restart.",
    startup_failed_prefix: "dsh web failed to start",
    launch_failed_prefix: "failed to launch dsh --profile web",
    unparseable_url_prefix: "dsh web announced an unparseable URL",
};
