use std::net::SocketAddr;

use clap::Args;

/// Limits for the optional HTTP coordinator; CLI jobs keep their existing behavior.
#[derive(Args, Clone, Debug)]
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
}

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
