use std::{path::PathBuf, sync::Arc, time::Instant};

use tokio::sync::{Semaphore, watch};
use tokio_util::task::TaskTracker;

use super::Config;

pub struct State {
    pub config: Config,
    pub executable: PathBuf,
    pub token: Option<String>,
    pub started: Instant,
    pub workers: Arc<Semaphore>,
    pub memory: Arc<Semaphore>,
    pub admission: Arc<Semaphore>,
    pub stop: watch::Sender<bool>,
    pub tasks: TaskTracker,
}

impl State {
    pub fn new(config: Config, executable: PathBuf, token: Option<String>) -> Arc<Self> {
        let workers = usize::from(config.max_workers);
        let capacity = workers + usize::from(config.max_queue);
        let memory = Arc::new(Semaphore::new(config.memory_budget_mib as usize));
        Arc::new(Self {
            memory,
            config,
            executable,
            token,
            started: Instant::now(),
            workers: Arc::new(Semaphore::new(workers)),
            admission: Arc::new(Semaphore::new(capacity)),
            stop: watch::channel(false).0,
            tasks: TaskTracker::new(),
        })
    }

    pub fn stopping(&self) -> bool {
        *self.stop.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::FromArgMatches;

    #[test]
    fn capacity_includes_workers_and_waiters() {
        let config = Config::from_arg_matches(&super::super::command().get_matches_from(["serve"]))
            .expect("config");
        let state = State::new(config, "pdf-toolkit".into(), None);
        assert_eq!(state.workers.available_permits(), 8);
        assert_eq!(state.admission.available_permits(), 40);
        assert!(!state.stopping());
        state.stop.send_replace(true);
        assert!(state.stopping());
    }
}
