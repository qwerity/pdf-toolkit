//! Bounded HTTP connections with socket write deadlines and graceful draining.
use super::{http, state::State, write_timeout::WriteTimeout};
use anyhow::{Context, Result};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};
use tokio_util::task::TaskTracker;

/// Accept bounded HTTP connections until shutdown, then drain connections and workers.
pub async fn serve(
    listener: TcpListener,
    state: Arc<State>,
    signal: impl Future<Output = ()>,
) -> Result<()> {
    let connections = TaskTracker::new();
    let slots = Arc::new(Semaphore::new(usize::from(state.config.max_connections)));
    let router = http::router(state.clone());
    tokio::pin!(signal);
    let result = loop {
        let accepted = tokio::select! {
            _ = &mut signal => break Ok(()),
            result = listener.accept() => result,
        };
        let (stream, _) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => break Err(error).context("cannot accept HTTP connection"),
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let state = state.clone();
        let router = router.clone();
        connections.spawn(async move {
            let _permit = permit;
            connection(stream, router, &state).await;
        });
    };
    drop(listener);
    state.stop.send_replace(true);
    connections.close();
    connections.wait().await;
    state.tasks.close();
    state.tasks.wait().await;
    result
}

async fn connection(stream: TcpStream, router: axum::Router, state: &State) {
    let mut stop = state.stop.subscribe();
    let io = TokioIo::new(WriteTimeout::new(
        stream,
        Duration::from_secs(state.config.write_timeout_seconds),
    ));
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10));
    let connection = builder.serve_connection(io, TowerToHyperService::new(router));
    tokio::pin!(connection);
    if !*stop.borrow_and_update() {
        tokio::select! {
            _ = &mut connection => return,
            _ = stop.changed() => {},
        }
    }
    connection.as_mut().graceful_shutdown();
    let _ = tokio::time::timeout(
        Duration::from_secs(state.config.shutdown_grace_seconds + 2),
        connection,
    )
    .await;
}
