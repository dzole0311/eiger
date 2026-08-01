use std::{env, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use serde::Serialize;
use thiserror::Error;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:3000";
const DEFAULT_MAX_CONCURRENT_SESSIONS: usize = 4;
const DEFAULT_PER_SESSION_RSS_MB: u64 = 1536;
const DEFAULT_RATE_LIMIT_RPS: u32 = 10;
const DEFAULT_RATE_LIMIT_BURST: u32 = 20;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub auth: AuthConfig,
    pub http: HttpConfig,
    pub rate_limit: RateLimitConfig,
    pub browser: BrowserConfig,
    pub pool: PoolConfig,
    pub stealth: StealthConfig,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfig {
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpConfig {
    pub cors_origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitConfig {
    pub requests_per_second: u32,
    pub burst: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConfig {
    pub executable: Option<PathBuf>,
    pub no_sandbox: bool,
    pub launch_timeout: Duration,
    pub close_timeout: Duration,
    pub terminate_timeout: Duration,
    pub additional_args: Vec<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolConfig {
    pub max_concurrent_sessions: usize,
    pub launch_queue_timeout: Duration,
    pub max_session_lifetime: Duration,
    pub max_idle_time: Duration,
    pub per_session_rss_limit_bytes: u64,
    pub maintenance_interval: Duration,
    pub cdp_health_interval: Duration,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StealthConfig {
    pub enabled: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid value for {name}: {value}")]
    InvalidValue { name: &'static str, value: String },
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_addr = parse_env("EIGER_BIND_ADDR", DEFAULT_BIND_ADDR)?;
        let token = optional_env("EIGER_TOKEN");
        let cors_origins = comma_separated_env("EIGER_CORS_ORIGINS");
        let rate_limit_rps = positive_u32_env("EIGER_RATE_LIMIT_RPS", DEFAULT_RATE_LIMIT_RPS)?;
        let rate_limit_burst =
            positive_u32_env("EIGER_RATE_LIMIT_BURST", DEFAULT_RATE_LIMIT_BURST)?;
        let executable = optional_env("EIGER_CHROME_EXECUTABLE").map(PathBuf::from);
        let no_sandbox = parse_env("EIGER_CHROME_NO_SANDBOX", "true")?;
        let launch_timeout = seconds_env("EIGER_BROWSER_LAUNCH_TIMEOUT_SECS", 10)?;
        let close_timeout = seconds_env("EIGER_BROWSER_CLOSE_TIMEOUT_SECS", 3)?;
        let terminate_timeout = seconds_env("EIGER_BROWSER_TERMINATE_TIMEOUT_SECS", 2)?;
        let additional_args = optional_env("EIGER_CHROME_ARGS")
            .map(|value| {
                value
                    .split_whitespace()
                    .filter(|part| !part.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let user_agent = optional_env("EIGER_USER_AGENT");

        let max_concurrent_sessions = parse_env(
            "EIGER_MAX_CONCURRENT_SESSIONS",
            &DEFAULT_MAX_CONCURRENT_SESSIONS.to_string(),
        )?;
        let launch_queue_timeout = seconds_env("EIGER_LAUNCH_QUEUE_TIMEOUT_SECS", 15)?;
        let max_session_lifetime = seconds_env("EIGER_MAX_SESSION_LIFETIME_SECS", 30 * 60)?;
        let max_idle_time = seconds_env("EIGER_MAX_IDLE_TIME_SECS", 5 * 60)?;
        let per_session_rss_limit_bytes =
            mb_env("EIGER_PER_SESSION_RSS_LIMIT_MB", DEFAULT_PER_SESSION_RSS_MB)? * 1024 * 1024;
        let maintenance_interval = seconds_env("EIGER_MAINTENANCE_INTERVAL_SECS", 5)?;
        let cdp_health_interval = seconds_env("EIGER_CDP_HEALTH_INTERVAL_SECS", 30)?;
        let stealth_enabled = parse_env("EIGER_STEALTH_ENABLED", "true")?;

        Ok(Self {
            bind_addr,
            auth: AuthConfig { token },
            http: HttpConfig { cors_origins },
            rate_limit: RateLimitConfig {
                requests_per_second: rate_limit_rps,
                burst: rate_limit_burst,
            },
            browser: BrowserConfig {
                executable,
                no_sandbox,
                launch_timeout,
                close_timeout,
                terminate_timeout,
                additional_args,
                user_agent,
            },
            pool: PoolConfig {
                max_concurrent_sessions,
                launch_queue_timeout,
                max_session_lifetime,
                max_idle_time,
                per_session_rss_limit_bytes,
                maintenance_interval,
                cdp_health_interval,
            },
            stealth: StealthConfig {
                enabled: stealth_enabled,
            },
        })
    }
}

fn optional_env(name: &'static str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn comma_separated_env(name: &'static str) -> Vec<String> {
    optional_env(name)
        .map(|value| comma_separated(&value))
        .unwrap_or_default()
}

fn comma_separated(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_env<T>(name: &'static str, default: &str) -> Result<T, ConfigError>
where
    T: FromStr,
{
    let value = env::var(name).unwrap_or_else(|_| default.to_owned());
    value
        .parse()
        .map_err(|_| ConfigError::InvalidValue { name, value })
}

fn seconds_env(name: &'static str, default: u64) -> Result<Duration, ConfigError> {
    parse_env::<u64>(name, &default.to_string()).map(Duration::from_secs)
}

fn mb_env(name: &'static str, default: u64) -> Result<u64, ConfigError> {
    parse_env(name, &default.to_string())
}

fn positive_u32_env(name: &'static str, default: u32) -> Result<u32, ConfigError> {
    let value = parse_env::<u32>(name, &default.to_string())?;
    if value == 0 {
        return Err(ConfigError::InvalidValue {
            name,
            value: value.to_string(),
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_for_single_node_self_hosting() {
        let config = AppConfig {
            bind_addr: DEFAULT_BIND_ADDR.parse().unwrap(),
            auth: AuthConfig { token: None },
            http: HttpConfig {
                cors_origins: Vec::new(),
            },
            rate_limit: RateLimitConfig {
                requests_per_second: DEFAULT_RATE_LIMIT_RPS,
                burst: DEFAULT_RATE_LIMIT_BURST,
            },
            browser: BrowserConfig {
                executable: None,
                no_sandbox: true,
                launch_timeout: Duration::from_secs(10),
                close_timeout: Duration::from_secs(3),
                terminate_timeout: Duration::from_secs(2),
                additional_args: Vec::new(),
                user_agent: None,
            },
            pool: PoolConfig {
                max_concurrent_sessions: DEFAULT_MAX_CONCURRENT_SESSIONS,
                launch_queue_timeout: Duration::from_secs(15),
                max_session_lifetime: Duration::from_secs(30 * 60),
                max_idle_time: Duration::from_secs(5 * 60),
                per_session_rss_limit_bytes: DEFAULT_PER_SESSION_RSS_MB * 1024 * 1024,
                maintenance_interval: Duration::from_secs(5),
                cdp_health_interval: Duration::from_secs(30),
            },
            stealth: StealthConfig { enabled: true },
        };

        assert_eq!(config.pool.max_concurrent_sessions, 4);
        assert_eq!(config.pool.per_session_rss_limit_bytes, 1536 * 1024 * 1024);
        assert_eq!(config.rate_limit.requests_per_second, 10);
        assert_eq!(config.rate_limit.burst, 20);
        assert!(config.browser.no_sandbox);
        assert!(config.stealth.enabled);
    }

    #[test]
    fn comma_separated_env_values_are_trimmed() {
        assert_eq!(
            comma_separated("https://app.example, https://admin.example ,,"),
            vec!["https://app.example", "https://admin.example"]
        );
    }
}
