use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use eiger_browser::{
    BrowserDriver, BrowserError, ChromeLaunchOptions, LaunchedBrowser, ShutdownKind,
    health_check_browser_ws, launch_chrome,
};
use eiger_config::AppConfig;
use eiger_metrics::{PoolMetrics, SessionMetric, sample_process_tree};
use eiger_stealth::StealthProfile;
use serde::Serialize;
use thiserror::Error;
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore},
    time::timeout,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    Launching,
    Ready,
    InUse,
    Draining,
    Killing,
    Dead,
}

impl fmt::Display for SessionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Launching => "launching",
            Self::Ready => "ready",
            Self::InUse => "in_use",
            Self::Draining => "draining",
            Self::Killing => "killing",
            Self::Dead => "dead",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionOverrides {
    pub stealth_enabled: Option<bool>,
    pub extra_chrome_args: Vec<String>,
    pub proxy: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: Uuid,
    pub state: SessionState,
    pub pid: Option<u32>,
    #[serde(skip_serializing)]
    pub browser_ws_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub age_seconds: u64,
    pub idle_seconds: u64,
    pub rss_bytes: Option<u64>,
    pub cpu_percent: Option<f32>,
    pub process_count: Option<usize>,
    pub kill_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolReadiness {
    pub status: &'static str,
    pub can_accept_sessions: bool,
    pub active_sessions: usize,
    pub max_concurrent_sessions: usize,
    pub available_capacity: usize,
    pub browser_launch_ready: bool,
    pub last_browser_launch_error: Option<String>,
}

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("session pool is at capacity")]
    AtCapacity,
    #[error("timed out waiting for browser capacity")]
    CapacityTimeout,
    #[error("session not found: {0}")]
    NotFound(Uuid),
    #[error("session is not connectable: {0}")]
    NotConnectable(Uuid),
    #[error("browser launch failed: {0}")]
    Browser(#[from] BrowserError),
}

#[derive(Debug)]
pub struct SessionPool {
    sessions: DashMap<Uuid, Arc<SessionEntry>>,
    semaphore: Arc<Semaphore>,
    options: PoolOptions,
    browser_options: ChromeLaunchOptions,
    sessions_created_total: AtomicU64,
    sessions_hard_killed_total: AtomicU64,
    sessions_rejected_total: AtomicU64,
    sessions_rss_limit_recycled_total: AtomicU64,
    sessions_idle_recycled_total: AtomicU64,
    sessions_lifetime_recycled_total: AtomicU64,
    sessions_process_exit_recycled_total: AtomicU64,
    sessions_cdp_unhealthy_recycled_total: AtomicU64,
    browser_launch_ready: AtomicBool,
    last_browser_launch_error: RwLock<Option<String>>,
}

#[derive(Debug, Clone)]
struct PoolOptions {
    max_concurrent_sessions: usize,
    launch_queue_timeout: Duration,
    max_session_lifetime: Duration,
    max_idle_time: Duration,
    per_session_rss_limit_bytes: u64,
    maintenance_interval: Duration,
    cdp_health_interval: Duration,
}

#[derive(Debug)]
struct SessionEntry {
    id: Uuid,
    pid: u32,
    browser_ws_url: String,
    stealth_enabled: bool,
    created_at: DateTime<Utc>,
    created_instant: Instant,
    last_used_at: RwLock<DateTime<Utc>>,
    last_used_instant: Mutex<Instant>,
    last_cdp_health_check: Mutex<Instant>,
    state: RwLock<SessionState>,
    browser: Mutex<Option<LaunchedBrowser>>,
    kill_reason: RwLock<Option<String>>,
    _permit: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub struct SessionHandle {
    entry: Arc<SessionEntry>,
}

impl SessionPool {
    pub fn from_config(config: &AppConfig) -> Arc<Self> {
        Arc::new(Self {
            sessions: DashMap::new(),
            semaphore: Arc::new(Semaphore::new(config.pool.max_concurrent_sessions)),
            options: PoolOptions {
                max_concurrent_sessions: config.pool.max_concurrent_sessions,
                launch_queue_timeout: config.pool.launch_queue_timeout,
                max_session_lifetime: config.pool.max_session_lifetime,
                max_idle_time: config.pool.max_idle_time,
                per_session_rss_limit_bytes: config.pool.per_session_rss_limit_bytes,
                maintenance_interval: config.pool.maintenance_interval,
                cdp_health_interval: config.pool.cdp_health_interval,
            },
            browser_options: ChromeLaunchOptions {
                executable: config.browser.executable.clone(),
                no_sandbox: config.browser.no_sandbox,
                launch_timeout: config.browser.launch_timeout,
                close_timeout: config.browser.close_timeout,
                terminate_timeout: config.browser.terminate_timeout,
                additional_args: config.browser.additional_args.clone(),
                proxy: None,
                stealth: StealthProfile::new(
                    config.stealth.enabled,
                    config.browser.user_agent.clone(),
                ),
            },
            sessions_created_total: AtomicU64::new(0),
            sessions_hard_killed_total: AtomicU64::new(0),
            sessions_rejected_total: AtomicU64::new(0),
            sessions_rss_limit_recycled_total: AtomicU64::new(0),
            sessions_idle_recycled_total: AtomicU64::new(0),
            sessions_lifetime_recycled_total: AtomicU64::new(0),
            sessions_process_exit_recycled_total: AtomicU64::new(0),
            sessions_cdp_unhealthy_recycled_total: AtomicU64::new(0),
            browser_launch_ready: AtomicBool::new(false),
            last_browser_launch_error: RwLock::new(None),
        })
    }

    pub fn spawn_maintenance(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.options.maintenance_interval);
            loop {
                interval.tick().await;
                self.maintenance_pass().await;
            }
        })
    }

    pub async fn create_session(
        &self,
        overrides: SessionOverrides,
    ) -> Result<SessionHandle, PoolError> {
        let permit = match timeout(
            self.options.launch_queue_timeout,
            self.semaphore.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                self.sessions_rejected_total.fetch_add(1, Ordering::Relaxed);
                return Err(PoolError::AtCapacity);
            }
            Err(_) => {
                self.sessions_rejected_total.fetch_add(1, Ordering::Relaxed);
                return Err(PoolError::CapacityTimeout);
            }
        };

        let mut launch_options = self.browser_options.clone();
        if let Some(enabled) = overrides.stealth_enabled {
            launch_options.stealth.enabled = enabled;
        }
        launch_options.proxy = overrides.proxy;
        launch_options
            .additional_args
            .extend(overrides.extra_chrome_args);

        let stealth_enabled = launch_options.stealth.enabled;
        let browser = match launch_chrome(launch_options).await {
            Ok(browser) => {
                self.browser_launch_ready.store(true, Ordering::Relaxed);
                *self.last_browser_launch_error.write().await = None;
                browser
            }
            Err(error) => {
                self.browser_launch_ready.store(false, Ordering::Relaxed);
                *self.last_browser_launch_error.write().await = Some(error.to_string());
                return Err(PoolError::Browser(error));
            }
        };
        let id = Uuid::new_v4();
        let now = Utc::now();
        let instant = Instant::now();
        let entry = Arc::new(SessionEntry {
            id,
            pid: browser.pid(),
            browser_ws_url: browser.browser_ws_url().to_owned(),
            stealth_enabled,
            created_at: now,
            created_instant: instant,
            last_used_at: RwLock::new(now),
            last_used_instant: Mutex::new(instant),
            last_cdp_health_check: Mutex::new(instant),
            state: RwLock::new(SessionState::Ready),
            browser: Mutex::new(Some(browser)),
            kill_reason: RwLock::new(None),
            _permit: permit,
        });

        self.sessions.insert(id, entry.clone());
        self.sessions_created_total.fetch_add(1, Ordering::Relaxed);
        info!(%id, pid = entry.pid, "browser session ready");

        Ok(SessionHandle { entry })
    }

    pub async fn connect_session(&self, id: Uuid) -> Result<SessionHandle, PoolError> {
        let entry = self
            .sessions
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or(PoolError::NotFound(id))?;

        let mut state = entry.state.write().await;
        match *state {
            SessionState::Ready => {
                *state = SessionState::InUse;
            }
            _ => return Err(PoolError::NotConnectable(id)),
        }
        drop(state);

        entry.touch().await;
        Ok(SessionHandle { entry })
    }

    pub async fn terminate_session(&self, id: Uuid, reason: impl Into<String>) -> bool {
        let reason = reason.into();
        let Some((_, entry)) = self.sessions.remove(&id) else {
            return false;
        };

        self.shutdown_entry(entry, reason).await;
        true
    }

    pub async fn get_session(&self, id: Uuid) -> Option<SessionInfo> {
        let entry = self.sessions.get(&id).map(|entry| entry.value().clone())?;
        Some(session_info(entry).await)
    }

    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let entries: Vec<_> = self
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        let mut sessions = Vec::with_capacity(entries.len());
        for entry in entries {
            sessions.push(session_info(entry).await);
        }
        sessions
    }

    pub async fn metrics(&self) -> PoolMetrics {
        let sessions = self
            .list_sessions()
            .await
            .into_iter()
            .map(|session| SessionMetric {
                id: session.id.to_string(),
                state: session.state.to_string(),
                pid: session.pid,
                rss_bytes: session.rss_bytes,
                cpu_percent: session.cpu_percent,
                process_count: session.process_count,
                age_seconds: session.age_seconds,
                idle_seconds: session.idle_seconds,
            })
            .collect();

        PoolMetrics {
            active_sessions: self.sessions.len(),
            max_concurrent_sessions: self.options.max_concurrent_sessions,
            sessions_created_total: self.sessions_created_total.load(Ordering::Relaxed),
            sessions_hard_killed_total: self.sessions_hard_killed_total.load(Ordering::Relaxed),
            sessions_rejected_total: self.sessions_rejected_total.load(Ordering::Relaxed),
            sessions_rss_limit_recycled_total: self
                .sessions_rss_limit_recycled_total
                .load(Ordering::Relaxed),
            sessions_idle_recycled_total: self.sessions_idle_recycled_total.load(Ordering::Relaxed),
            sessions_lifetime_recycled_total: self
                .sessions_lifetime_recycled_total
                .load(Ordering::Relaxed),
            sessions_process_exit_recycled_total: self
                .sessions_process_exit_recycled_total
                .load(Ordering::Relaxed),
            sessions_cdp_unhealthy_recycled_total: self
                .sessions_cdp_unhealthy_recycled_total
                .load(Ordering::Relaxed),
            sessions,
        }
    }

    pub async fn readiness(&self) -> PoolReadiness {
        let available_capacity = self.semaphore.available_permits();
        let browser_launch_ready = self.browser_launch_ready.load(Ordering::Relaxed);
        let can_accept_sessions = available_capacity > 0 && browser_launch_ready;

        PoolReadiness {
            status: if can_accept_sessions {
                "ready"
            } else {
                "not_ready"
            },
            can_accept_sessions,
            active_sessions: self.sessions.len(),
            max_concurrent_sessions: self.options.max_concurrent_sessions,
            available_capacity,
            browser_launch_ready,
            last_browser_launch_error: self.last_browser_launch_error.read().await.clone(),
        }
    }

    async fn maintenance_pass(&self) {
        let entries: Vec<_> = self
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .collect();

        for entry in entries {
            if matches!(
                *entry.state.read().await,
                SessionState::Killing | SessionState::Dead
            ) {
                continue;
            }

            if let Some(reason) = self.kill_reason_for(&entry).await {
                warn!(%reason, id = %entry.id, pid = entry.pid, "maintenance recycling session");
                self.terminate_session(entry.id, reason).await;
            }
        }
    }

    async fn kill_reason_for(&self, entry: &SessionEntry) -> Option<String> {
        if entry.created_instant.elapsed() > self.options.max_session_lifetime {
            return Some("max session lifetime exceeded".to_owned());
        }

        let state = entry.state.read().await.clone();
        let idle_for = entry.last_used_instant.lock().await.elapsed();
        if state == SessionState::Ready && idle_for > self.options.max_idle_time {
            return Some("max idle time exceeded".to_owned());
        }

        match sample_process_tree(entry.pid).await {
            Some(sample) if sample.rss_bytes > self.options.per_session_rss_limit_bytes => {
                return Some(format!(
                    "rss limit exceeded: {} > {}",
                    sample.rss_bytes, self.options.per_session_rss_limit_bytes
                ));
            }
            Some(_) => {}
            None => return Some("browser process exited".to_owned()),
        }

        let mut last_cdp_health_check = entry.last_cdp_health_check.lock().await;
        if last_cdp_health_check.elapsed() >= self.options.cdp_health_interval {
            *last_cdp_health_check = Instant::now();
            drop(last_cdp_health_check);
            if let Err(error) =
                health_check_browser_ws(&entry.browser_ws_url, Duration::from_secs(2)).await
            {
                return Some(format!("cdp health check failed: {error}"));
            }
            debug!(id = %entry.id, pid = entry.pid, "cdp health check passed");
        }

        None
    }

    async fn shutdown_entry(&self, entry: Arc<SessionEntry>, reason: String) {
        {
            let mut state = entry.state.write().await;
            *state = SessionState::Killing;
            *entry.kill_reason.write().await = Some(reason.clone());
        }
        self.count_recycle_reason(&reason);

        let browser = entry.browser.lock().await.take();
        if let Some(browser) = browser {
            let report = browser.shutdown(reason).await;
            if report.kind == ShutdownKind::Killed {
                self.sessions_hard_killed_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            info!(
                id = %entry.id,
                pid = report.pid,
                kind = ?report.kind,
                reason = %report.reason,
                "browser session stopped"
            );
        }

        *entry.state.write().await = SessionState::Dead;
    }

    fn count_recycle_reason(&self, reason: &str) {
        let counter = if reason.starts_with("rss limit exceeded") {
            Some(&self.sessions_rss_limit_recycled_total)
        } else if reason.starts_with("max idle time exceeded") {
            Some(&self.sessions_idle_recycled_total)
        } else if reason.starts_with("max session lifetime exceeded") {
            Some(&self.sessions_lifetime_recycled_total)
        } else if reason.starts_with("browser process exited") {
            Some(&self.sessions_process_exit_recycled_total)
        } else if reason.starts_with("cdp health check failed") {
            Some(&self.sessions_cdp_unhealthy_recycled_total)
        } else {
            None
        };

        if let Some(counter) = counter {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl SessionHandle {
    pub fn id(&self) -> Uuid {
        self.entry.id
    }

    pub fn pid(&self) -> u32 {
        self.entry.pid
    }

    pub fn browser_ws_url(&self) -> &str {
        &self.entry.browser_ws_url
    }

    pub fn stealth_enabled(&self) -> bool {
        self.entry.stealth_enabled
    }

    pub async fn touch(&self) {
        self.entry.touch().await;
    }

    pub async fn mark_in_use(&self) {
        *self.entry.state.write().await = SessionState::InUse;
        self.touch().await;
    }
}

impl SessionEntry {
    async fn touch(&self) {
        let now = Utc::now();
        *self.last_used_at.write().await = now;
        *self.last_used_instant.lock().await = Instant::now();
    }
}

async fn session_info(entry: Arc<SessionEntry>) -> SessionInfo {
    let state = entry.state.read().await.clone();
    let last_used_at = *entry.last_used_at.read().await;
    let kill_reason = entry.kill_reason.read().await.clone();
    let resource_sample = sample_process_tree(entry.pid).await;

    SessionInfo {
        id: entry.id,
        state,
        pid: Some(entry.pid),
        browser_ws_url: Some(entry.browser_ws_url.clone()),
        created_at: entry.created_at,
        last_used_at,
        age_seconds: entry.created_instant.elapsed().as_secs(),
        idle_seconds: entry.last_used_instant.lock().await.elapsed().as_secs(),
        rss_bytes: resource_sample.as_ref().map(|sample| sample.rss_bytes),
        cpu_percent: resource_sample.as_ref().map(|sample| sample.cpu_percent),
        process_count: resource_sample.as_ref().map(|sample| sample.process_count),
        kill_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eiger_config::{
        AuthConfig, BrowserConfig, HttpConfig, PoolConfig, RateLimitConfig, StealthConfig,
    };

    #[tokio::test]
    async fn create_session_times_out_when_launch_queue_has_no_capacity() {
        let config = test_config(0, Duration::from_millis(10));
        let pool = SessionPool::from_config(&config);
        let started_at = Instant::now();

        let error = match pool.create_session(SessionOverrides::default()).await {
            Ok(_) => panic!("request should time out before launch"),
            Err(error) => error,
        };

        assert!(matches!(error, PoolError::CapacityTimeout));
        assert!(started_at.elapsed() < Duration::from_secs(1));
    }

    fn test_config(max_concurrent_sessions: usize, launch_queue_timeout: Duration) -> AppConfig {
        AppConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            auth: AuthConfig { token: None },
            http: HttpConfig {
                cors_origins: Vec::new(),
                request_body_limit_bytes: 64 * 1024,
            },
            rate_limit: RateLimitConfig {
                requests_per_second: 10,
                burst: 20,
            },
            browser: BrowserConfig {
                executable: None,
                no_sandbox: false,
                launch_timeout: Duration::from_secs(10),
                close_timeout: Duration::from_secs(3),
                terminate_timeout: Duration::from_secs(2),
                additional_args: Vec::new(),
                user_agent: None,
            },
            pool: PoolConfig {
                max_concurrent_sessions,
                launch_queue_timeout,
                max_session_lifetime: Duration::from_secs(30 * 60),
                max_idle_time: Duration::from_secs(5 * 60),
                per_session_rss_limit_bytes: 1536 * 1024 * 1024,
                maintenance_interval: Duration::from_secs(5),
                cdp_health_interval: Duration::from_secs(30),
            },
            stealth: StealthConfig { enabled: true },
        }
    }
}
