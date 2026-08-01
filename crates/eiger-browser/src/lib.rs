use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use eiger_stealth::{StealthProfile, baseline_scripts};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use thiserror::Error;
use tokio::{
    fs,
    process::{Child, Command},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct ChromeLaunchOptions {
    pub executable: Option<PathBuf>,
    pub no_sandbox: bool,
    pub launch_timeout: Duration,
    pub close_timeout: Duration,
    pub terminate_timeout: Duration,
    pub additional_args: Vec<String>,
    pub proxy: Option<String>,
    pub stealth: StealthProfile,
}

#[derive(Debug)]
pub struct LaunchedBrowser {
    child: Child,
    pid: u32,
    http_base_url: String,
    browser_ws_url: String,
    _user_data_dir: TempDir,
    stealth_task: Option<JoinHandle<()>>,
    close_timeout: Duration,
    terminate_timeout: Duration,
}

pub trait BrowserDriver {
    fn pid(&self) -> u32;
    fn http_base_url(&self) -> &str;
    fn browser_ws_url(&self) -> &str;
}

#[derive(Debug, Clone)]
pub struct BrowserReady {
    pub pid: u32,
    pub http_base_url: String,
    pub browser_ws_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownKind {
    Graceful,
    Terminated,
    Killed,
    AlreadyExited,
}

#[derive(Debug, Clone)]
pub struct ShutdownReport {
    pub pid: u32,
    pub kind: ShutdownKind,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("chrome executable could not be resolved; set EIGER_CHROME_EXECUTABLE")]
    MissingExecutable,
    #[error("failed to spawn chrome at {path}: {source}")]
    Spawn {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("chrome exited before CDP became ready: {0}")]
    ExitedEarly(String),
    #[error("chrome did not expose CDP within {0:?}")]
    LaunchTimeout(Duration),
    #[error("failed to read DevToolsActivePort: {0}")]
    DevtoolsPort(std::io::Error),
    #[error("invalid DevToolsActivePort contents: {0}")]
    InvalidDevtoolsPort(String),
    #[error("CDP HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("CDP websocket failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("CDP command timed out: {0}")]
    CdpTimeout(&'static str),
    #[error("CDP command failed: {0}")]
    CdpCommand(String),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionResponse {
    web_socket_debugger_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetDescriptor {
    #[serde(rename = "type")]
    target_type: String,
    web_socket_debugger_url: Option<String>,
}

pub async fn launch_chrome(options: ChromeLaunchOptions) -> Result<LaunchedBrowser, BrowserError> {
    let executable = resolve_chrome_executable(options.executable.as_deref())?;
    let user_data_dir = tempfile::Builder::new()
        .prefix("eiger-chrome-")
        .tempdir()
        .map_err(BrowserError::DevtoolsPort)?;

    let mut command = Command::new(&executable);
    command
        .args(chrome_args(user_data_dir.path(), &options))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);

    let mut child = command.spawn().map_err(|source| BrowserError::Spawn {
        path: executable.display().to_string(),
        source,
    })?;

    let pid = child.id().ok_or(BrowserError::MissingExecutable)?;
    let ready = match wait_until_ready(
        pid,
        &mut child,
        user_data_dir.path(),
        options.launch_timeout,
    )
    .await
    {
        Ok(ready) => ready,
        Err(error) => {
            terminate_child(&mut child, pid, options.terminate_timeout).await;
            return Err(error);
        }
    };

    let browser = LaunchedBrowser {
        child,
        pid,
        http_base_url: ready.http_base_url,
        browser_ws_url: ready.browser_ws_url,
        _user_data_dir: user_data_dir,
        stealth_task: None,
        close_timeout: options.close_timeout,
        terminate_timeout: options.terminate_timeout,
    };

    let mut browser = browser;
    if options.stealth.enabled {
        if let Err(error) = browser.inject_stealth_into_existing_targets().await {
            warn!(%error, pid = browser.pid, "failed to inject baseline stealth scripts into existing targets");
        }
        browser.start_stealth_controller();
    }

    Ok(browser)
}

impl LaunchedBrowser {
    pub async fn inject_stealth_into_existing_targets(&self) -> Result<(), BrowserError> {
        let client = Client::new();
        let targets: Vec<TargetDescriptor> = client
            .get(format!("{}/json/list", self.http_base_url))
            .send()
            .await?
            .json()
            .await?;

        for target in targets {
            if !matches!(target.target_type.as_str(), "page" | "iframe") {
                continue;
            }

            let Some(ws_url) = target.web_socket_debugger_url else {
                continue;
            };

            for script in baseline_scripts() {
                cdp_command(&ws_url, "Page.enable", json!({}), Duration::from_secs(2)).await?;
                cdp_command(
                    &ws_url,
                    "Page.addScriptToEvaluateOnNewDocument",
                    json!({ "source": script.source, "runImmediately": true }),
                    Duration::from_secs(2),
                )
                .await?;
                cdp_command(
                    &ws_url,
                    "Runtime.evaluate",
                    json!({ "expression": script.source }),
                    Duration::from_secs(2),
                )
                .await?;
                debug!(
                    script = script.name,
                    pid = self.pid,
                    "injected stealth script"
                );
            }
        }

        Ok(())
    }

    fn start_stealth_controller(&mut self) {
        let ws_url = self.browser_ws_url.clone();
        let pid = self.pid;
        self.stealth_task = Some(tokio::spawn(async move {
            if let Err(error) = run_stealth_controller(&ws_url).await {
                debug!(%error, pid, "stealth controller stopped");
            }
        }));
    }

    pub async fn shutdown(mut self, reason: impl Into<String>) -> ShutdownReport {
        let reason = reason.into();
        let pid = self.pid;
        if let Some(task) = self.stealth_task.take() {
            task.abort();
        }

        if let Ok(Some(_)) = self.child.try_wait() {
            return ShutdownReport {
                pid,
                kind: ShutdownKind::AlreadyExited,
                reason,
            };
        }

        let browser_close_succeeded = timeout(
            self.close_timeout,
            cdp_command(
                &self.browser_ws_url,
                "Browser.close",
                json!({}),
                self.close_timeout,
            ),
        )
        .await
        .is_ok_and(|result| result.is_ok());

        if browser_close_succeeded
            && timeout(self.terminate_timeout, self.child.wait())
                .await
                .is_ok()
        {
            return ShutdownReport {
                pid,
                kind: ShutdownKind::Graceful,
                reason,
            };
        }

        send_sigterm(pid);

        if timeout(self.terminate_timeout, self.child.wait())
            .await
            .is_ok()
        {
            return ShutdownReport {
                pid,
                kind: ShutdownKind::Terminated,
                reason,
            };
        }

        if let Err(error) = self.child.kill().await {
            warn!(%error, pid, "failed to hard-kill chrome child");
        }

        ShutdownReport {
            pid,
            kind: ShutdownKind::Killed,
            reason,
        }
    }
}

impl BrowserDriver for LaunchedBrowser {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn http_base_url(&self) -> &str {
        &self.http_base_url
    }

    fn browser_ws_url(&self) -> &str {
        &self.browser_ws_url
    }
}

async fn terminate_child(child: &mut Child, pid: u32, terminate_timeout: Duration) {
    if !matches!(child.try_wait(), Ok(None)) {
        return;
    }

    send_sigterm(pid);

    if timeout(terminate_timeout, child.wait()).await.is_ok() {
        return;
    }

    if let Err(error) = child.kill().await {
        warn!(%error, pid, "failed to clean up chrome after launch failure");
    }
}

impl Drop for LaunchedBrowser {
    fn drop(&mut self) {
        if let Some(task) = self.stealth_task.take() {
            task.abort();
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.start_kill();
        }
    }
}

pub async fn health_check_browser_ws(ws_url: &str, wait: Duration) -> Result<(), BrowserError> {
    cdp_command(ws_url, "Target.getTargets", json!({}), wait)
        .await
        .map(|_| ())
}

pub async fn cdp_command(
    ws_url: &str,
    method: &'static str,
    params: Value,
    wait: Duration,
) -> Result<Value, BrowserError> {
    let (mut socket, _) = connect_async(ws_url).await?;
    let request = json!({
        "id": 1,
        "method": method,
        "params": params,
    });

    socket
        .send(Message::Text(request.to_string().into()))
        .await?;

    let response = timeout(wait, async {
        while let Some(message) = socket.next().await {
            let message = message?;
            let Message::Text(text) = message else {
                continue;
            };
            let value: Value = serde_json::from_str(&text)
                .map_err(|error| BrowserError::CdpCommand(error.to_string()))?;

            if value.get("id").and_then(Value::as_u64) != Some(1) {
                continue;
            }

            if let Some(error) = value.get("error") {
                return Err(BrowserError::CdpCommand(error.to_string()));
            }

            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }

        Err(BrowserError::CdpCommand("websocket closed".to_owned()))
    })
    .await
    .map_err(|_| BrowserError::CdpTimeout(method))??;

    let _ = socket.close(None).await;
    Ok(response)
}

async fn run_stealth_controller(ws_url: &str) -> Result<(), BrowserError> {
    let (mut socket, _) = connect_async(ws_url).await?;
    let mut next_id = 10_000_u64;

    send_cdp_message(
        &mut socket,
        &mut next_id,
        None,
        "Target.setAutoAttach",
        json!({
            "autoAttach": true,
            "waitForDebuggerOnStart": true,
            "flatten": true
        }),
    )
    .await?;

    send_cdp_message(
        &mut socket,
        &mut next_id,
        None,
        "Target.setDiscoverTargets",
        json!({ "discover": true }),
    )
    .await?;

    while let Some(message) = socket.next().await {
        let message = message?;
        let Message::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| BrowserError::CdpCommand(error.to_string()))?;

        if let Some(error) = value.get("error") {
            warn!(%error, "stealth controller CDP command failed");
        }

        let Some(method) = value.get("method").and_then(Value::as_str) else {
            continue;
        };

        match method {
            "Target.targetCreated" => {
                let Some(target_info) = value
                    .get("params")
                    .and_then(|params| params.get("targetInfo"))
                else {
                    continue;
                };
                let target_type = target_info.get("type").and_then(Value::as_str);
                let Some(target_id) = target_info.get("targetId").and_then(Value::as_str) else {
                    continue;
                };

                if matches!(target_type, Some("page" | "iframe")) {
                    send_cdp_message(
                        &mut socket,
                        &mut next_id,
                        None,
                        "Target.attachToTarget",
                        json!({
                            "targetId": target_id,
                            "flatten": true
                        }),
                    )
                    .await?;
                }
            }
            "Target.attachedToTarget" => {
                let Some(params) = value.get("params") else {
                    continue;
                };
                let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
                    continue;
                };
                let target_type = params
                    .get("targetInfo")
                    .and_then(|target| target.get("type"))
                    .and_then(Value::as_str);

                if !matches!(target_type, Some("page" | "iframe")) {
                    send_cdp_message(
                        &mut socket,
                        &mut next_id,
                        Some(session_id),
                        "Runtime.runIfWaitingForDebugger",
                        json!({}),
                    )
                    .await?;
                    continue;
                }

                for script in baseline_scripts() {
                    send_cdp_message(
                        &mut socket,
                        &mut next_id,
                        Some(session_id),
                        "Page.enable",
                        json!({}),
                    )
                    .await?;
                    send_cdp_message(
                        &mut socket,
                        &mut next_id,
                        Some(session_id),
                        "Page.addScriptToEvaluateOnNewDocument",
                        json!({ "source": script.source, "runImmediately": true }),
                    )
                    .await?;
                    send_cdp_message(
                        &mut socket,
                        &mut next_id,
                        Some(session_id),
                        "Runtime.evaluate",
                        json!({ "expression": script.source }),
                    )
                    .await?;
                }

                send_cdp_message(
                    &mut socket,
                    &mut next_id,
                    Some(session_id),
                    "Runtime.runIfWaitingForDebugger",
                    json!({}),
                )
                .await?;
            }
            _ => {}
        }
    }

    Ok(())
}

async fn send_cdp_message(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: &mut u64,
    session_id: Option<&str>,
    method: &'static str,
    params: Value,
) -> Result<(), BrowserError> {
    let id = *next_id;
    *next_id += 1;

    let mut request = json!({
        "id": id,
        "method": method,
        "params": params,
    });

    if let Some(session_id) = session_id {
        request["sessionId"] = Value::String(session_id.to_owned());
    }

    socket
        .send(Message::Text(request.to_string().into()))
        .await?;
    Ok(())
}

fn chrome_args(user_data_dir: &Path, options: &ChromeLaunchOptions) -> Vec<String> {
    let mut args = vec![
        "--headless=new".to_owned(),
        "--remote-debugging-address=127.0.0.1".to_owned(),
        "--remote-debugging-port=0".to_owned(),
        format!("--user-data-dir={}", user_data_dir.display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-background-networking".to_owned(),
        "--disable-background-timer-throttling".to_owned(),
        "--disable-client-side-phishing-detection".to_owned(),
        "--disable-component-update".to_owned(),
        "--disable-default-apps".to_owned(),
        "--disable-dev-shm-usage".to_owned(),
        "--disable-extensions".to_owned(),
        "--disable-gpu".to_owned(),
        "--disable-hang-monitor".to_owned(),
        "--disable-popup-blocking".to_owned(),
        "--disable-prompt-on-repost".to_owned(),
        "--disable-sync".to_owned(),
        "--metrics-recording-only".to_owned(),
        "--mute-audio".to_owned(),
        "--password-store=basic".to_owned(),
        "--use-mock-keychain".to_owned(),
    ];

    if options.no_sandbox {
        args.push("--no-sandbox".to_owned());
    }

    if let Some(proxy) = options
        .proxy
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        args.push(format!("--proxy-server={}", proxy.trim()));
    }

    args.extend(options.stealth.chrome_args());
    args.extend(options.additional_args.clone());
    args.push("about:blank".to_owned());
    args
}

async fn wait_until_ready(
    pid: u32,
    child: &mut Child,
    user_data_dir: &Path,
    launch_timeout: Duration,
) -> Result<BrowserReady, BrowserError> {
    let started_at = Instant::now();
    let client = Client::new();

    while started_at.elapsed() < launch_timeout {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| BrowserError::ExitedEarly(error.to_string()))?
        {
            return Err(BrowserError::ExitedEarly(status.to_string()));
        }

        match read_devtools_port(user_data_dir).await {
            Ok(Some(port)) => {
                let http_base_url = format!("http://127.0.0.1:{port}");
                match client
                    .get(format!("{http_base_url}/json/version"))
                    .send()
                    .await
                {
                    Ok(response) if response.status().is_success() => {
                        let version: VersionResponse = response.json().await?;
                        health_check_browser_ws(
                            &version.web_socket_debugger_url,
                            Duration::from_secs(2),
                        )
                        .await?;
                        return Ok(BrowserReady {
                            pid,
                            http_base_url,
                            browser_ws_url: version.web_socket_debugger_url,
                        });
                    }
                    Ok(_) | Err(_) => {}
                }
            }
            Ok(None) => {}
            Err(error) => return Err(error),
        }

        sleep(Duration::from_millis(100)).await;
    }

    Err(BrowserError::LaunchTimeout(launch_timeout))
}

async fn read_devtools_port(user_data_dir: &Path) -> Result<Option<u16>, BrowserError> {
    let path = user_data_dir.join("DevToolsActivePort");
    let contents = match fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(BrowserError::DevtoolsPort(error)),
    };

    let first_line = contents
        .lines()
        .next()
        .ok_or_else(|| BrowserError::InvalidDevtoolsPort(contents.clone()))?;

    first_line
        .parse::<u16>()
        .map(Some)
        .map_err(|_| BrowserError::InvalidDevtoolsPort(contents))
}

fn resolve_chrome_executable(configured: Option<&Path>) -> Result<PathBuf, BrowserError> {
    if let Some(path) = configured {
        return Ok(path.to_path_buf());
    }

    for env_name in ["CHROME", "CHROME_BIN", "GOOGLE_CHROME_BIN"] {
        if let Ok(value) = env::var(env_name)
            && !value.trim().is_empty()
        {
            return Ok(PathBuf::from(value));
        }
    }

    for path in [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ] {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    for binary in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
    ] {
        if binary_is_on_path(binary) {
            return Ok(PathBuf::from(binary));
        }
    }

    Err(BrowserError::MissingExecutable)
}

fn binary_is_on_path(binary: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths).any(|path| path.join(binary).is_file())
}

#[cfg(unix)]
fn send_sigterm(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn send_sigterm(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_args_include_resource_saving_defaults() {
        let options = ChromeLaunchOptions {
            executable: None,
            no_sandbox: true,
            launch_timeout: Duration::from_secs(10),
            close_timeout: Duration::from_secs(3),
            terminate_timeout: Duration::from_secs(2),
            additional_args: Vec::new(),
            proxy: Some("http://proxy.local:8080".to_owned()),
            stealth: StealthProfile::new(true, None),
        };

        let temp_dir = tempfile::tempdir().unwrap();
        let args = chrome_args(temp_dir.path(), &options);

        assert!(args.contains(&"--remote-debugging-port=0".to_owned()));
        assert!(args.contains(&"--disable-dev-shm-usage".to_owned()));
        assert!(args.contains(&"--no-sandbox".to_owned()));
        assert!(args.contains(&"--proxy-server=http://proxy.local:8080".to_owned()));
        assert!(!args.contains(&"--enable-automation".to_owned()));
    }

    #[test]
    fn chrome_args_omit_proxy_when_unset() {
        let options = ChromeLaunchOptions {
            executable: None,
            no_sandbox: false,
            launch_timeout: Duration::from_secs(10),
            close_timeout: Duration::from_secs(3),
            terminate_timeout: Duration::from_secs(2),
            additional_args: Vec::new(),
            proxy: None,
            stealth: StealthProfile::new(false, None),
        };

        let temp_dir = tempfile::tempdir().unwrap();
        let args = chrome_args(temp_dir.path(), &options);

        assert!(!args.iter().any(|arg| arg.starts_with("--proxy-server=")));
    }
}
