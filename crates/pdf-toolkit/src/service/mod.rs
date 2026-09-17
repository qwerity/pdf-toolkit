//! Optional HTTP coordinator. PDF processing stays in disposable CLI children.

mod config;
mod error;
mod http;
mod memory;
mod request;
mod response;
mod state;
mod transport;
mod worker;
mod write_timeout;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use tokio::sync::watch;

pub use config::{Config, command};
pub use worker::{downsample_budget, watch_parent};

/// Run until SIGINT, SIGTERM or POST /shutdown; finish/reap jobs before returning.
pub fn run(config: Config) -> Result<()> {
    config.validate()?;
    if config.print_config {
        return crate::cli_output::print_json(&config);
    }
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        bail!("worker memory monitoring is unsupported on this operating system");
    }
    let token = std::env::var("PDF_TOOLKIT_SERVICE_TOKEN")
        .ok()
        .filter(|token| !token.is_empty());
    validate_binding(&config, token.as_deref())?;
    let executable = std::env::current_exe().context("cannot locate worker executable")?;
    let state = state::State::new(config, executable, token);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .thread_keep_alive(Duration::from_secs(1))
        .enable_all()
        .build()
        .context("cannot create HTTP runtime")?;
    let result = runtime.block_on(serve(state));
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

fn validate_binding(config: &Config, token: Option<&str>) -> Result<()> {
    if !config.bind.ip().is_loopback() && token.is_none() {
        bail!("non-loopback listeners require PDF_TOOLKIT_SERVICE_TOKEN");
    }
    Ok(())
}

async fn serve(state: Arc<state::State>) -> Result<()> {
    let signal = shutdown_signal(state.stop.subscribe())?;
    let listener = tokio::net::TcpListener::bind(state.config.bind)
        .await
        .with_context(|| format!("cannot listen on {}", state.config.bind))?;
    eprintln!(
        "pdf-toolkit service listening on {} (max workers: {})",
        listener
            .local_addr()
            .context("cannot read listener address")?,
        state.config.max_workers
    );
    transport::serve(listener, state, signal).await
}

#[cfg(unix)]
fn shutdown_signal(mut stop: watch::Receiver<bool>) -> Result<impl Future<Output = ()> + Send> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt =
        signal(SignalKind::interrupt()).context("cannot register SIGINT handler")?;
    let mut terminate =
        signal(SignalKind::terminate()).context("cannot register SIGTERM handler")?;
    Ok(async move {
        if *stop.borrow_and_update() {
            return;
        }
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
            _ = stop.changed() => {},
        }
    })
}

#[cfg(not(unix))]
fn shutdown_signal(mut stop: watch::Receiver<bool>) -> Result<impl Future<Output = ()> + Send> {
    Ok(async move {
        if *stop.borrow_and_update() {
            return;
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = stop.changed() => {},
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::FromArgMatches;

    #[test]
    fn remote_binding_requires_authentication() {
        let config = Config::from_arg_matches(&command().get_matches_from([
            "serve",
            "--bind",
            "0.0.0.0:8787",
        ]))
        .expect("config");
        assert!(validate_binding(&config, None).is_err());
        assert!(validate_binding(&config, Some("secret")).is_ok());
    }
}
