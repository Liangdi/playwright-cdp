//! Chrome/Chromium process management: executable discovery, spawning with
//! `--remote-debugging-port`, DevToolsActivePort discovery, and `/json/version`
//! resolution to the browser WebSocket endpoint.

use crate::error::{Error, Result};
use crate::options::LaunchOptions;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

/// The result of launching a browser: the owning process handle plus the
/// browser-level WebSocket endpoint to connect to.
pub struct BrowserProcess {
    child: Option<Child>,
    _user_data_dir: tempfile::TempDir,
    ws_url: String,
    stderr_buf: std::sync::Arc<parking_lot::Mutex<String>>,
}

impl BrowserProcess {
    /// Spawn a Chromium-based browser per `opts` and discover its DevTools
    /// WebSocket endpoint. Blocks until the browser is ready (or `timeout`).
    pub async fn launch(opts: &LaunchOptions) -> Result<Self> {
        let executable = discover_executable(opts)
            .map_err(|e| e.context("could not find a Chrome/Chromium executable"))?;

        let user_data_dir = tempfile::Builder::new()
            .prefix("playwright-cdp-")
            .tempdir()
            .map_err(|e| Error::Io(e).context("failed to create user-data-dir"))?;

        let mut args = base_args(user_data_dir.path(), opts);
        if opts.headless.unwrap_or(true) {
            args.push("--headless=new".to_string());
        }
        if opts.devtools.unwrap_or(false) {
            args.push("--auto-open-devtools-for-tabs".to_string());
        }
        if let Some(proxy) = &opts.proxy {
            args.push(format!("--proxy-server={}", proxy.server));
        }
        // User args last so they can override defaults.
        if let Some(extra) = &opts.args {
            args.extend(extra.iter().cloned());
        }

        tracing::debug!(exe = %executable.display(), args = ?args, "launching browser");

        let stderr_buf = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
        let stderr_for_task = stderr_buf.clone();

        let mut cmd = Command::new(&executable);
        cmd.args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // On Linux, sandbox requires CAP_SYS_ADMIN; disable it when running as root (CI).
        #[cfg(target_os = "linux")]
        if unsafe { libc_geteuid() } == 0 {
            // already passed? ensure --no-sandbox present
            if !args.iter().any(|a| a == "--no-sandbox") {
                cmd.arg("--no-sandbox");
            }
        }
        // Propagate env overrides.
        if let Some(env) = &opts.env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::LaunchFailed(format!("failed to spawn {executable:?}: {e}")))?;

        // Drain stderr in the background, keeping the most recent ~8KB.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                drain_stderr(stderr, stderr_for_task).await;
            });
        }

        let timeout_ms = opts.timeout_ms_or_default() as u64;
        let port = wait_for_devtools_port(user_data_dir.path(), timeout_ms).await.map_err(|e| {
            let stderr_snapshot = stderr_buf.lock().clone();
            Error::LaunchFailed(format!(
                "{e}\n--- browser stderr (tail) ---\n{}",
                stderr_snapshot.chars().rev().take(4096).collect::<String>().chars().rev().collect::<String>()
            ))
        })?;

        let ws_url = fetch_browser_ws_url(port, timeout_ms).await?;

        Ok(Self {
            child: Some(child),
            _user_data_dir: user_data_dir,
            ws_url,
            stderr_buf,
        })
    }

    /// The browser-level WebSocket endpoint (`ws://host:port/devtools/browser/<id>`).
    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    /// Send SIGKILL (best-effort) and wait for exit.
    pub async fn kill(&mut self) -> Result<()> {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        Ok(())
    }

    /// Recent stderr output (for diagnostics).
    pub fn stderr_tail(&self) -> String {
        self.stderr_buf.lock().clone()
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        // kill_on_drop(true) on the Command handles SIGKILL; this is a fallback.
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

async fn drain_stderr<R: tokio::io::AsyncRead + Unpin>(
    mut stderr: R,
    buf: std::sync::Arc<parking_lot::Mutex<String>>,
) {
    let mut tmp = [0u8; 1024];
    loop {
        match stderr.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let s = String::from_utf8_lossy(&tmp[..n]).to_string();
                let mut guard = buf.lock();
                guard.push_str(&s);
                // Keep only the last ~16KB.
                const MAX: usize = 16 * 1024;
                if guard.len() > MAX {
                    let drop = guard.len() - MAX;
                    guard.drain(..drop);
                }
            }
        }
    }
}

fn base_args(user_data_dir: &std::path::Path, _opts: &LaunchOptions) -> Vec<String> {
    vec![
        format!(
            "--user-data-dir={}",
            user_data_dir.display()
        ),
        "--remote-debugging-port=0".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-background-networking".to_string(),
        "--password-store=basic".to_string(),
        "--use-mock-keychain".to_string(),
        "--disable-features=Translate,IsolateOrigins,site-per-process".to_string(),
        "about:blank".to_string(),
    ]
}

/// Poll `<user-data-dir>/DevToolsActivePort` until it appears and parses.
///
/// The file is `"<port>\n<path>"`. It is written asynchronously once Chrome is
/// ready, and may be momentarily empty/truncated, so we read-and-parse in a loop.
async fn wait_for_devtools_port(user_data_dir: &std::path::Path, timeout_ms: u64) -> Result<u16> {
    let path = user_data_dir.join("DevToolsActivePort");
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms.max(1000));
    loop {
        if let Ok(contents) = tokio::fs::read_to_string(&path).await {
            if let Some(first_line) = contents.lines().next() {
                if let Ok(port) = first_line.trim().parse::<u16>() {
                    if port != 0 {
                        return Ok(port);
                    }
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::LaunchFailed(format!(
                "timed out after {timeout_ms}ms waiting for DevToolsActivePort at {}",
                path.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// `GET http://127.0.0.1:<port>/json/version` and extract `webSocketDebuggerUrl`.
async fn fetch_browser_ws_url(port: u16, timeout_ms: u64) -> Result<String> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms.max(1000));
    loop {
        match reqwest::get(&url).await {
            Ok(resp) if resp.status().is_success() => {
                let v: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| Error::Http(format!("/json/version parse: {e}")))?;
                let ws = v
                    .get("webSocketDebuggerUrl")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| {
                        Error::ProtocolError("/json/version missing webSocketDebuggerUrl".into())
                    })?
                    .to_string();
                return Ok(ws);
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::ConnectionFailed(format!(
                "timed out waiting for {url}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Resolve an arbitrary CDP endpoint to a `ws://` URL.
///
/// - `http(s)://host:port` → `GET {endpoint}/json/version` → `webSocketDebuggerUrl`
/// - `ws(s)://...` → returned unchanged
pub async fn resolve_ws_endpoint(endpoint: &str) -> Result<String> {
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(endpoint.to_string());
    }
    let mut base = endpoint.trim_end_matches('/').to_string();
    base.push_str("/json/version");
    let resp = reqwest::get(&base)
        .await
        .map_err(|e| Error::Http(format!("GET {base}: {e}")))?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| Error::Http(format!("parse {base}: {e}")))?;
    v.get("webSocketDebuggerUrl")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::ProtocolError("response missing webSocketDebuggerUrl".into()))
}

// ---------------------------------------------------------------------------
// Executable discovery
// ---------------------------------------------------------------------------

pub(crate) fn discover_executable(opts: &LaunchOptions) -> Result<PathBuf> {
    if let Some(p) = &opts.executable_path {
        let path = PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
        return Err(Error::LaunchFailed(format!(
            "executable_path does not exist: {p}"
        )));
    }

    for var in ["PWC_CHROME_PATH", "CHROME_PATH"] {
        if let Ok(p) = std::env::var(var) {
            let path = PathBuf::from(&p);
            if path.exists() {
                return Ok(path);
            }
        }
    }

    for candidate in executable_candidates(opts.channel.as_deref()) {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(Error::LaunchFailed(
        "no Chrome/Chromium executable found; set executable_path, the CHROME_PATH env var, or channel".into(),
    ))
}

/// Ordered candidate executables for a channel (or all channels if `None`).
fn executable_candidates(channel: Option<&str>) -> Vec<PathBuf> {
    let channels: Vec<&str> = match channel {
        Some(c) => vec![c],
        None => vec!["chrome", "chromium", "msedge"],
    };

    let mut out = Vec::new();
    for ch in channels {
        out.extend(known_locations(ch));
        out.extend(path_lookup(ch));
    }
    out
}

/// `which`-style scan of `$PATH` for the channel's binary names.
fn path_lookup(channel: &str) -> Vec<PathBuf> {
    let names: &[&str] = match channel {
        "chrome" => &["google-chrome", "google-chrome-stable", "chrome"],
        "chromium" => &["chromium", "chromium-browser"],
        "msedge" => &["microsoft-edge", "microsoft-edge-stable"],
        _ => &[],
    };
    let path = match std::env::var_os("PATH") {
        Some(p) => std::env::split_paths(&p).collect::<Vec<_>>(),
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for name in names {
        for dir in &path {
            let candidate = dir.join(name);
            if candidate.is_file() {
                out.push(candidate);
            }
        }
    }
    out
}

/// Well-known absolute install locations per channel per OS.
fn known_locations(channel: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();

    match channel {
        "chrome" => {
            #[cfg(target_os = "linux")]
            out.extend([
                "/opt/google/chrome/chrome",
                "/usr/bin/google-chrome",
                "/usr/bin/google-chrome-stable",
                "/usr/bin/google-chrome-beta",
            ]);
            #[cfg(target_os = "macos")]
            out.extend([
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta",
            ]);
            #[cfg(target_os = "windows")]
            {
                for dir in windows_program_dirs() {
                    out.push(PathBuf::from(dir).join("Google\\Chrome\\Application\\chrome.exe"));
                }
            }
        }
        "chromium" => {
            #[cfg(target_os = "linux")]
            out.extend(["/usr/bin/chromium", "/usr/bin/chromium-browser", "/snap/bin/chromium"]);
            #[cfg(target_os = "macos")]
            out.push("/Applications/Chromium.app/Contents/MacOS/Chromium");
            #[cfg(target_os = "windows")]
            {
                for dir in windows_program_dirs() {
                    out.push(PathBuf::from(dir).join("Chromium\\Application\\chrome.exe"));
                }
            }
        }
        "msedge" => {
            #[cfg(target_os = "linux")]
            out.extend(["/usr/bin/microsoft-edge", "/opt/microsoft/edge/microsoft-edge"]);
            #[cfg(target_os = "macos")]
            out.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge");
            #[cfg(target_os = "windows")]
            {
                for dir in windows_program_dirs() {
                    out.push(PathBuf::from(dir).join("Microsoft\\Edge\\Application\\msedge.exe"));
                }
            }
        }
        _ => {}
    }

    out.into_iter().map(PathBuf::from).collect()
}

/// Program-file / local-app candidate roots on Windows.
#[cfg(target_os = "windows")]
fn windows_program_dirs() -> Vec<String> {
    let mut dirs = Vec::new();
    if let Ok(v) = std::env::var("ProgramFiles") {
        dirs.push(v);
    } else {
        dirs.push("C:\\Program Files".into());
    }
    if let Ok(v) = std::env::var("ProgramFiles(x86)") {
        dirs.push(v);
    }
    if let Ok(v) = std::env::var("LOCALAPPDATA") {
        dirs.push(v);
    }
    dirs
}

/// libc geteuid without taking a libc dependency (raw syscall on Linux).
#[cfg(target_os = "linux")]
unsafe fn libc_geteuid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_locations_returns_something_on_supported_platforms() {
        // On the supported platforms at least the chrome channel yields entries.
        let c = known_locations("chrome");
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        assert!(!c.is_empty(), "expected chrome known locations");
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let _ = c;
    }

    #[test]
    fn discover_respects_explicit_executable_path() {
        let opts = LaunchOptions::default().executable_path("/nonexistent/path/to/chrome-xyz");
        let r = discover_executable(&opts);
        assert!(r.is_err());
    }

    #[test]
    fn devtools_port_parses_well_formed_contents() {
        // The parser reads line 1 as the port; verify the parsing rule directly.
        let contents = "9333\n/devtools/browser/abc-123\n";
        let port: u16 = contents
            .lines()
            .next()
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(port, 9333);
    }
}
