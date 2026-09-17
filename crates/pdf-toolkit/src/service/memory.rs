//! Per-job reservations plus resident-memory enforcement for disposable workers.
use super::error::ApiError;
use axum::http::StatusCode;
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

struct Probe {
    system: System,
    pid: Pid,
}

impl Probe {
    fn new(pid: u32) -> Self {
        Self {
            system: System::new(),
            pid: Pid::from_u32(pid),
        }
    }

    fn resident_bytes(&mut self) -> Option<u64> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_memory().without_tasks(),
        );
        self.system
            .process(self.pid)
            .map(|process| process.memory())
    }
}

/// Resolve when the child exceeds its limit. Sampling is not a kernel hard cap:
/// transient allocations can overshoot between samples. Reservations ensure that
/// admitted worker ceilings sum to at most the configured service budget.
pub async fn exceeded(pid: u32, limit_mib: u32) -> ApiError {
    let mut probe = Probe::new(pid);
    loop {
        let result = tokio::task::spawn_blocking(move || {
            let bytes = probe.resident_bytes();
            (probe, bytes)
        })
        .await;
        let (returned, bytes) = match result {
            Ok(result) => result,
            Err(error) => {
                return ApiError::internal(format!("cannot monitor worker memory: {error}"));
            }
        };
        probe = returned;
        if let Some(error) = check(bytes, limit_mib) {
            return error;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn check(bytes: Option<u64>, limit_mib: u32) -> Option<ApiError> {
    bytes
        .filter(|bytes| *bytes > u64::from(limit_mib) * 1024 * 1024)
        .map(|bytes| {
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("worker exceeded memory limit of {limit_mib} MiB (observed {bytes} bytes)"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_use_bytes_and_do_not_misclassify_exited_processes() {
        assert!(check(None, 1).is_none());
        assert!(check(Some(1024 * 1024), 1).is_none());
        assert!(check(Some(1024 * 1024 + 1), 1).is_some());
        let mut probe = Probe::new(std::process::id());
        assert!(probe.resident_bytes().expect("current process memory") > 0);
    }
}
