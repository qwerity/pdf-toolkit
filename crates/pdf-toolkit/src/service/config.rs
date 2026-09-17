use std::net::SocketAddr;

use anyhow::{Result, ensure};
use clap::Args;
use serde::Serialize;

/// Limits for the optional HTTP coordinator; CLI jobs keep their existing behavior.
#[derive(Args, Clone, Debug, Serialize)]
pub struct Config {
    /// Private HTTP listener. Non-loopback addresses require PDF_TOOLKIT_SERVICE_TOKEN.
    #[arg(long, default_value = "127.0.0.1:8787")]
    pub bind: SocketAddr,
    /// Maximum simultaneously executing CLI processes (zero idle workers).
    #[arg(long, default_value = "8", value_parser = clap::value_parser!(u16).range(1..=1024))]
    pub max_workers: u16,
    /// Additional admitted requests, including waiting and streaming responses.
    #[arg(long, default_value = "32", value_parser = clap::value_parser!(u16).range(0..=4096))]
    pub max_queue: u16,
    /// Total seconds allowed for queueing and execution of one request.
    #[arg(long, default_value = "3600", value_parser = clap::value_parser!(u64).range(1..=86400))]
    pub timeout_seconds: u64,
    /// Seconds to allow running jobs to finish after SIGINT, SIGTERM or /shutdown.
    #[arg(long, default_value = "30", value_parser = clap::value_parser!(u64).range(0..=86400))]
    pub shutdown_grace_seconds: u64,
    /// Rayon threads per CLI worker, to prevent nested CPU oversubscription.
    #[arg(long, default_value = "1", value_parser = clap::value_parser!(u16).range(1..=1024))]
    pub worker_threads: u16,
    /// Close a connection when response writes make no progress for this long.
    #[arg(long, default_value = "30", value_parser = clap::value_parser!(u64).range(1..=86400))]
    pub write_timeout_seconds: u64,
    /// Maximum open HTTP connections, including idle connections.
    #[arg(long, default_value = "128", value_parser = clap::value_parser!(u16).range(1..=65535))]
    pub max_connections: u16,
    /// Total MiB reserved for active workers; each reserves worker-memory-mib.
    #[arg(long, default_value = "8192", value_parser = clap::value_parser!(u32).range(1..=1048576))]
    pub memory_budget_mib: u32,
    /// Per-worker resident-memory ceiling, checked every 100 ms.
    #[arg(long, default_value = "1024", value_parser = clap::value_parser!(u32).range(1..=1048576))]
    pub worker_memory_mib: u32,
    /// Maximum pixels per rendered page, including dossier and VLM renders.
    #[arg(long, default_value = "32000000", value_parser = clap::value_parser!(u64).range(1..=1000000000))]
    pub max_render_pixels: u64,
    /// Print validated, resolved configuration as JSON and exit without listening.
    #[arg(long)]
    #[serde(skip)]
    pub print_config: bool,
}

impl Config {
    /// Validate limits that depend on each other before starting any workers.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.worker_memory_mib <= self.memory_budget_mib,
            "--worker-memory-mib cannot exceed --memory-budget-mib"
        );
        Ok(())
    }
}

/// Build the service subcommand, shared by runtime parsing and configuration inspection.
pub fn command() -> clap::Command {
    Config::augment_args(clap::Command::new("serve").about("Run the private JSON HTTP service"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::FromArgMatches;

    #[test]
    fn defaults_and_worker_override() {
        let defaults = Config::from_arg_matches(&command().get_matches_from(["serve"]))
            .expect("default config");
        assert_eq!(defaults.max_workers, 8);
        let changed =
            Config::from_arg_matches(&command().get_matches_from(["serve", "--max-workers", "16"]))
                .expect("override");
        assert_eq!(changed.max_workers, 16);
    }

    #[test]
    fn memory_reservations_must_fit_the_total_budget() {
        let config = Config::from_arg_matches(&command().get_matches_from([
            "serve",
            "--memory-budget-mib",
            "512",
            "--worker-memory-mib",
            "1024",
        ]))
        .expect("parse limits");
        assert!(config.validate().is_err());
        assert!(
            Config::from_arg_matches(&command().get_matches_from(["serve"]))
                .expect("defaults")
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn rejects_invalid_limits() {
        for args in [
            ["serve", "--max-workers", "0"],
            ["serve", "--max-workers", "-1"],
            ["serve", "--max-workers", "65536"],
            ["serve", "--timeout-seconds", "0"],
            ["serve", "--worker-threads", "0"],
        ] {
            assert!(command().try_get_matches_from(args).is_err());
        }
    }
}
