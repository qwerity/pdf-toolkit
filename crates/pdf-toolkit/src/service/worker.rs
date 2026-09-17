use std::{
    io::Read,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use axum::http::StatusCode;
use tokio::{
    fs::File,
    process::Child,
    sync::{OwnedSemaphorePermit, oneshot},
    time::Instant,
};

use super::{error::ApiError, memory, response::error_excerpt, state::State};

const PARENT_PIPE_ENV: &str = "PDF_TOOLKIT_PARENT_PIPE";

/// In service children only, stdin is a lifetime lease, not document input.
/// EOF means the coordinator died; exiting releases native allocations even
/// after SIGKILL of the coordinator. Normal CLI invocation is unaffected.
pub fn watch_parent() -> Result<()> {
    if std::env::var_os(PARENT_PIPE_ENV).as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("parent-lifetime".into())
        .spawn(|| {
            let mut byte = [0];
            loop {
                match std::io::stdin().read(&mut byte) {
                    Ok(0) => std::process::exit(125),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => std::process::exit(125),
                    Ok(_) => {}
                }
            }
        })
        .context("cannot start worker parent monitor")?;
    Ok(())
}

/// Read the coordinator's per-worker resampling limit; ordinary CLI runs use library defaults.
pub fn downsample_budget() -> Result<Option<u64>> {
    match std::env::var("PDF_TOOLKIT_DOWNSAMPLE_BYTES") {
        Ok(raw) => Ok(Some(parse_budget(&raw)?)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).context("cannot read resampling memory budget"),
    }
}

fn parse_budget(raw: &str) -> Result<u64> {
    let bytes = raw
        .parse::<u64>()
        .context("invalid resampling memory budget")?;
    anyhow::ensure!(bytes > 0, "resampling memory budget must be positive");
    Ok(bytes)
}

pub struct JobOutput {
    pub stdout: File,
    pub admission: OwnedSemaphorePermit,
}

/// A supervisor owns the child and permits even if the HTTP handler disappears.
/// Permits are released only after exit/reaping, or after response delivery.
pub fn submit(
    state: Arc<State>,
    args: Vec<String>,
    admission: OwnedSemaphorePermit,
    deadline: Instant,
) -> oneshot::Receiver<Result<JobOutput, ApiError>> {
    let (mut sender, receiver) = oneshot::channel();
    state.tasks.clone().spawn(async move {
        let result = execute(&state, &args, &mut sender, deadline).await;
        let _ = sender.send(result.map(|stdout| JobOutput { stdout, admission }));
    });
    receiver
}

async fn execute(
    state: &State,
    args: &[String],
    sender: &mut oneshot::Sender<Result<JobOutput, ApiError>>,
    deadline: Instant,
) -> Result<File, ApiError> {
    let (_memory, _worker) = acquire(state, sender, deadline).await?;
    let stdout = tempfile::tempfile().map_err(ApiError::internal)?;
    let stderr = tempfile::tempfile().map_err(ApiError::internal)?;
    let mut child = spawn(state, args, &stdout, &stderr)?;
    // Child::wait closes its stdin; take ownership to keep the lifetime pipe open.
    let _parent_lease = child.stdin.take();
    let result = wait(&mut child, state, sender, deadline).await;
    // Even a wait error must not leave work running after releasing its slot.
    if result.is_err() {
        child.kill().await.map_err(ApiError::internal)?;
    }
    let status = result?;
    if !status.success() {
        let diagnostic = error_excerpt(File::from_std(stderr)).await?;
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("worker exited with {status}: {diagnostic}"),
        ));
    }
    Ok(File::from_std(stdout))
}

async fn acquire<'a>(
    state: &'a State,
    sender: &mut oneshot::Sender<Result<JobOutput, ApiError>>,
    deadline: Instant,
) -> Result<
    (
        tokio::sync::SemaphorePermit<'a>,
        tokio::sync::SemaphorePermit<'a>,
    ),
    ApiError,
> {
    let mut stop = state.stop.subscribe();
    if state.stopping() {
        return Err(ApiError::unavailable("service is stopping"));
    }
    tokio::select! {
        biased;
        _ = stop.changed() => Err(ApiError::unavailable("service is stopping")),
        _ = sender.closed() => Err(ApiError::unavailable("caller disconnected")),
        _ = tokio::time::sleep_until(deadline) => Err(ApiError::timeout()),
        permits = async {
            let memory = state.memory.acquire_many(state.config.worker_memory_mib).await.map_err(ApiError::internal)?;
            let worker = state.workers.acquire().await.map_err(ApiError::internal)?;
            Ok((memory, worker))
        } => permits,
    }
}

fn spawn(
    state: &State,
    args: &[String],
    stdout: &std::fs::File,
    stderr: &std::fs::File,
) -> Result<Child, ApiError> {
    tokio::process::Command::new(&state.executable)
        .args(args)
        .env(PARENT_PIPE_ENV, "1")
        .env("RAYON_NUM_THREADS", state.config.worker_threads.to_string())
        .env_remove("PDF_TOOLKIT_SERVICE_TOKEN")
        .env(
            "PDF_TOOLKIT_MAX_RENDER_PIXELS",
            state.config.max_render_pixels.to_string(),
        )
        .env(
            "PDF_TOOLKIT_DOWNSAMPLE_BYTES",
            (u64::from(state.config.worker_memory_mib) * 1024 * 1024 / 4)
                .min(256 * 1024 * 1024)
                .to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(stdout.try_clone().map_err(ApiError::internal)?)
        .stderr(stderr.try_clone().map_err(ApiError::internal)?)
        .kill_on_drop(true)
        .spawn()
        .map_err(ApiError::internal)
}

async fn wait(
    child: &mut Child,
    state: &State,
    sender: &mut oneshot::Sender<Result<JobOutput, ApiError>>,
    deadline: Instant,
) -> Result<ExitStatus, ApiError> {
    let pid = child
        .id()
        .ok_or_else(|| ApiError::internal("worker has no process ID"))?;
    tokio::select! {
        status = child.wait() => status.map_err(ApiError::internal),
        _ = sender.closed() => Err(ApiError::unavailable("caller disconnected")),
        _ = tokio::time::sleep_until(deadline) => Err(ApiError::timeout()),
        error = memory::exceeded(pid, state.config.worker_memory_mib) => Err(error),
        _ = shutdown_deadline(state) => Err(ApiError::unavailable("shutdown grace expired")),
    }
}

async fn shutdown_deadline(state: &State) {
    let mut stop = state.stop.subscribe();
    if !*stop.borrow_and_update() {
        let _ = stop.changed().await;
    }
    tokio::time::sleep(Duration::from_secs(state.config.shutdown_grace_seconds)).await;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use clap::FromArgMatches;

    fn state() -> Arc<State> {
        let config =
            super::super::Config::from_arg_matches(&super::super::command().get_matches_from([
                "serve",
                "--max-workers",
                "1",
                "--shutdown-grace-seconds",
                "0",
            ]))
            .expect("config");
        State::new(config, "/bin/sh".into(), None)
    }

    async fn start(
        state: &Arc<State>,
        script: &str,
        duration: Duration,
    ) -> oneshot::Receiver<Result<JobOutput, ApiError>> {
        let admission = state
            .admission
            .clone()
            .acquire_owned()
            .await
            .expect("permit");
        submit(
            state.clone(),
            vec!["-c".into(), script.into()],
            admission,
            Instant::now() + duration,
        )
    }

    #[test]
    fn memory_budget_requires_positive_bytes() {
        assert_eq!(parse_budget("1024").expect("budget"), 1024);
        for raw in ["0", "-1", "invalid"] {
            assert!(parse_budget(raw).is_err());
        }
    }

    #[tokio::test]
    async fn worker_success_and_failure_release_slots() {
        let state = state();
        let result = start(&state, "printf '{}'", Duration::from_secs(5))
            .await
            .await
            .expect("supervisor");
        assert!(result.is_ok());
        drop(result);
        let result = start(
            &state,
            "printf 'bad input' >&2; exit 1",
            Duration::from_secs(5),
        )
        .await
        .await
        .expect("supervisor");
        assert_eq!(
            result.err().expect("failure").0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        state.tasks.close();
        state.tasks.wait().await;
        assert_eq!(state.workers.available_permits(), 1);
        assert_eq!(state.admission.available_permits(), 33);
    }

    #[tokio::test]
    async fn memory_reservations_limit_concurrency_and_release_on_timeout() {
        let mut config =
            super::super::Config::from_arg_matches(&super::super::command().get_matches_from([
                "serve",
                "--max-workers",
                "8",
            ]))
            .expect("config");
        config.memory_budget_mib = 1024;
        let state = State::new(config, "/bin/sh".into(), None);
        let first = start(&state, "exec sleep 30", Duration::from_millis(150)).await;
        while state.workers.available_permits() == 8 {
            tokio::task::yield_now().await;
        }
        let second = start(&state, "printf '{}'", Duration::from_secs(5)).await;
        assert_eq!(state.workers.available_permits(), 7);
        assert_eq!(state.memory.available_permits(), 0);
        assert_eq!(
            first.await.expect("supervisor").err().expect("timeout").0,
            StatusCode::GATEWAY_TIMEOUT
        );
        drop(second.await.expect("supervisor").expect("second job"));
        assert_eq!(state.memory.available_permits(), 1024);
    }

    #[tokio::test]
    async fn timeout_and_disconnect_reap_workers() {
        let state = state();
        let result = start(&state, "exec sleep 30", Duration::from_millis(50))
            .await
            .await
            .expect("supervisor");
        assert_eq!(
            result.err().expect("timeout").0,
            StatusCode::GATEWAY_TIMEOUT
        );
        let receiver = start(&state, "exec sleep 30", Duration::from_secs(5)).await;
        drop(receiver);
        state.tasks.close();
        tokio::time::timeout(Duration::from_secs(2), state.tasks.wait())
            .await
            .expect("workers reaped");
        assert_eq!(state.workers.available_permits(), 1);
    }

    #[tokio::test]
    async fn shutdown_cancels_running_and_queued_jobs() {
        let state = state();
        let running = start(&state, "exec sleep 30", Duration::from_secs(5)).await;
        while state.workers.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
        let queued = start(&state, "printf '{}'", Duration::from_secs(5)).await;
        state.stop.send_replace(true);
        for receiver in [running, queued] {
            assert_eq!(
                receiver
                    .await
                    .expect("supervisor")
                    .err()
                    .expect("stopped")
                    .0,
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
        assert_eq!(state.workers.available_permits(), 1);
    }
}
