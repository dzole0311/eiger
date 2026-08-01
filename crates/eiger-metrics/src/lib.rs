use serde::Serialize;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSample {
    pub pid: u32,
    pub rss_bytes: u64,
    pub cpu_percent: f32,
    pub process_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetric {
    pub id: String,
    pub state: String,
    pub pid: Option<u32>,
    pub rss_bytes: Option<u64>,
    pub cpu_percent: Option<f32>,
    pub process_count: Option<usize>,
    pub age_seconds: u64,
    pub idle_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolMetrics {
    pub active_sessions: usize,
    pub max_concurrent_sessions: usize,
    pub sessions_created_total: u64,
    pub sessions_hard_killed_total: u64,
    pub sessions_rejected_total: u64,
    pub sessions_rss_limit_recycled_total: u64,
    pub sessions_idle_recycled_total: u64,
    pub sessions_lifetime_recycled_total: u64,
    pub sessions_process_exit_recycled_total: u64,
    pub sessions_cdp_unhealthy_recycled_total: u64,
    pub sessions: Vec<SessionMetric>,
}

pub async fn sample_process(pid: u32) -> Option<ProcessSample> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-o", "pcpu=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find(|line| !line.trim().is_empty())?;
    let mut parts = line.split_whitespace();
    let rss_kb = parts.next()?.parse::<u64>().ok()?;
    let cpu_percent = parts.next()?.parse::<f32>().ok()?;

    Some(ProcessSample {
        pid,
        rss_bytes: rss_kb * 1024,
        cpu_percent,
        process_count: 1,
    })
}

pub async fn sample_process_tree(pid: u32) -> Option<ProcessSample> {
    let output = Command::new("ps")
        .args(["-axo", "pid=", "-o", "ppid=", "-o", "rss=", "-o", "pcpu="])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return sample_process(pid).await;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut rows = Vec::new();

    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let Some(child_pid) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let Some(parent_pid) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let Some(rss_kb) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
            continue;
        };
        let Some(cpu_percent) = parts.next().and_then(|value| value.parse::<f32>().ok()) else {
            continue;
        };

        rows.push((child_pid, parent_pid, rss_kb * 1024, cpu_percent));
    }

    if !rows.iter().any(|(child_pid, _, _, _)| *child_pid == pid) {
        return None;
    }

    let mut descendants = vec![pid];
    let mut cursor = 0;
    while cursor < descendants.len() {
        let current = descendants[cursor];
        cursor += 1;

        for (child_pid, parent_pid, _, _) in &rows {
            if *parent_pid == current && !descendants.contains(child_pid) {
                descendants.push(*child_pid);
            }
        }
    }

    let mut rss_bytes = 0;
    let mut cpu_percent = 0.0;
    let mut process_count = 0;

    for (child_pid, _, child_rss_bytes, child_cpu_percent) in rows {
        if descendants.contains(&child_pid) {
            rss_bytes += child_rss_bytes;
            cpu_percent += child_cpu_percent;
            process_count += 1;
        }
    }

    Some(ProcessSample {
        pid,
        rss_bytes,
        cpu_percent,
        process_count,
    })
}

pub fn render_prometheus(metrics: &PoolMetrics) -> String {
    let mut output = String::new();

    output.push_str("# HELP eiger_sessions_active Active browser sessions.\n");
    output.push_str("# TYPE eiger_sessions_active gauge\n");
    output.push_str(&format!(
        "eiger_sessions_active {}\n",
        metrics.active_sessions
    ));

    output.push_str("# HELP eiger_sessions_max Maximum concurrently allowed browser sessions.\n");
    output.push_str("# TYPE eiger_sessions_max gauge\n");
    output.push_str(&format!(
        "eiger_sessions_max {}\n",
        metrics.max_concurrent_sessions
    ));

    output.push_str("# HELP eiger_sessions_created_total Browser sessions created.\n");
    output.push_str("# TYPE eiger_sessions_created_total counter\n");
    output.push_str(&format!(
        "eiger_sessions_created_total {}\n",
        metrics.sessions_created_total
    ));

    output.push_str(
        "# HELP eiger_sessions_hard_killed_total Browser sessions that required SIGKILL.\n",
    );
    output.push_str("# TYPE eiger_sessions_hard_killed_total counter\n");
    output.push_str(&format!(
        "eiger_sessions_hard_killed_total {}\n",
        metrics.sessions_hard_killed_total
    ));

    output.push_str(
        "# HELP eiger_sessions_rejected_total Session requests rejected by capacity or timeout.\n",
    );
    output.push_str("# TYPE eiger_sessions_rejected_total counter\n");
    output.push_str(&format!(
        "eiger_sessions_rejected_total {}\n",
        metrics.sessions_rejected_total
    ));

    output.push_str("# HELP eiger_sessions_rss_limit_recycled_total Sessions recycled after exceeding their RSS ceiling.\n");
    output.push_str("# TYPE eiger_sessions_rss_limit_recycled_total counter\n");
    output.push_str(&format!(
        "eiger_sessions_rss_limit_recycled_total {}\n",
        metrics.sessions_rss_limit_recycled_total
    ));

    output.push_str(
        "# HELP eiger_sessions_idle_recycled_total Sessions recycled after idle timeout.\n",
    );
    output.push_str("# TYPE eiger_sessions_idle_recycled_total counter\n");
    output.push_str(&format!(
        "eiger_sessions_idle_recycled_total {}\n",
        metrics.sessions_idle_recycled_total
    ));

    output.push_str(
        "# HELP eiger_sessions_lifetime_recycled_total Sessions recycled after max lifetime.\n",
    );
    output.push_str("# TYPE eiger_sessions_lifetime_recycled_total counter\n");
    output.push_str(&format!(
        "eiger_sessions_lifetime_recycled_total {}\n",
        metrics.sessions_lifetime_recycled_total
    ));

    output.push_str("# HELP eiger_sessions_process_exit_recycled_total Sessions recycled because their browser process exited.\n");
    output.push_str("# TYPE eiger_sessions_process_exit_recycled_total counter\n");
    output.push_str(&format!(
        "eiger_sessions_process_exit_recycled_total {}\n",
        metrics.sessions_process_exit_recycled_total
    ));

    output.push_str("# HELP eiger_sessions_cdp_unhealthy_recycled_total Sessions recycled after CDP health-check failure.\n");
    output.push_str("# TYPE eiger_sessions_cdp_unhealthy_recycled_total counter\n");
    output.push_str(&format!(
        "eiger_sessions_cdp_unhealthy_recycled_total {}\n",
        metrics.sessions_cdp_unhealthy_recycled_total
    ));

    output.push_str("# HELP eiger_session_rss_bytes Browser process resident memory by session.\n");
    output.push_str("# TYPE eiger_session_rss_bytes gauge\n");
    for session in &metrics.sessions {
        if let Some(rss_bytes) = session.rss_bytes {
            output.push_str(&format!(
                "eiger_session_rss_bytes{{session=\"{}\",state=\"{}\"}} {}\n",
                escape_label(&session.id),
                escape_label(&session.state),
                rss_bytes
            ));
        }
    }

    output.push_str("# HELP eiger_session_cpu_percent Browser process CPU percent by session.\n");
    output.push_str("# TYPE eiger_session_cpu_percent gauge\n");
    for session in &metrics.sessions {
        if let Some(cpu_percent) = session.cpu_percent {
            output.push_str(&format!(
                "eiger_session_cpu_percent{{session=\"{}\",state=\"{}\"}} {}\n",
                escape_label(&session.id),
                escape_label(&session.state),
                cpu_percent
            ));
        }
    }

    output.push_str(
        "# HELP eiger_session_processes Browser process-tree process count by session.\n",
    );
    output.push_str("# TYPE eiger_session_processes gauge\n");
    for session in &metrics.sessions {
        if let Some(process_count) = session.process_count {
            output.push_str(&format!(
                "eiger_session_processes{{session=\"{}\",state=\"{}\"}} {}\n",
                escape_label(&session.id),
                escape_label(&session.state),
                process_count
            ));
        }
    }

    output
}

fn escape_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_render_includes_counters_and_session_rss() {
        let metrics = PoolMetrics {
            active_sessions: 1,
            max_concurrent_sessions: 4,
            sessions_created_total: 2,
            sessions_hard_killed_total: 1,
            sessions_rejected_total: 3,
            sessions_rss_limit_recycled_total: 4,
            sessions_idle_recycled_total: 5,
            sessions_lifetime_recycled_total: 6,
            sessions_process_exit_recycled_total: 7,
            sessions_cdp_unhealthy_recycled_total: 8,
            sessions: vec![SessionMetric {
                id: "abc".to_owned(),
                state: "ready".to_owned(),
                pid: Some(42),
                rss_bytes: Some(1000),
                cpu_percent: Some(0.5),
                process_count: Some(3),
                age_seconds: 10,
                idle_seconds: 1,
            }],
        };

        let rendered = render_prometheus(&metrics);

        assert!(rendered.contains("eiger_sessions_active 1"));
        assert!(rendered.contains("eiger_sessions_hard_killed_total 1"));
        assert!(rendered.contains("eiger_sessions_rss_limit_recycled_total 4"));
        assert!(rendered.contains("eiger_session_rss_bytes{session=\"abc\",state=\"ready\"} 1000"));
        assert!(rendered.contains("eiger_session_processes{session=\"abc\",state=\"ready\"} 3"));
    }
}
