use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Request, State as ExtractState},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::time::Instant;

use super::{
    error::ApiError,
    request::{JobRequest, validate},
    response::json_file,
    state::State,
    worker,
};

pub fn router(state: Arc<State>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/status", get(status))
        .route("/shutdown", post(shutdown))
        .route("/v1/{command}", post(job))
        .method_not_allowed_fallback(|| async {
            ApiError(StatusCode::METHOD_NOT_ALLOWED, "method not allowed".into())
        })
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "unknown endpoint".into()) })
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

async fn authenticate(
    ExtractState(state): ExtractState<Arc<State>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(token) = &state.token {
        let supplied = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if !supplied.is_some_and(|supplied| token_matches(supplied.as_bytes(), token.as_bytes())) {
            return ApiError(
                StatusCode::UNAUTHORIZED,
                "valid bearer token required".into(),
            )
            .into_response();
        }
    }
    next.run(request).await
}

fn token_matches(supplied: &[u8], token: &[u8]) -> bool {
    supplied.len() == token.len()
        && supplied
            .iter()
            .zip(token)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn ready(ExtractState(state): ExtractState<Arc<State>>) -> Result<Json<Value>, ApiError> {
    if state.stopping() {
        return Err(ApiError::unavailable("service is stopping"));
    }
    Ok(Json(json!({"status": "ready"})))
}

async fn status(ExtractState(state): ExtractState<Arc<State>>) -> Json<Value> {
    let workers = usize::from(state.config.max_workers);
    let capacity = workers + usize::from(state.config.max_queue);
    Json(json!({
        "pid": std::process::id(), "version": env!("CARGO_PKG_VERSION"),
        "uptime_seconds": state.started.elapsed().as_secs(),
        "state": if state.stopping() { "stopping" } else { "ready" },
        "max_workers": workers, "max_queue": state.config.max_queue,
        "active_workers": workers - state.workers.available_permits(),
        "outstanding_requests": capacity - state.admission.available_permits(),
        "worker_threads": state.config.worker_threads,
        "memory_budget_mib": state.config.memory_budget_mib,
        "worker_memory_mib": state.config.worker_memory_mib,
        "reserved_memory_mib": state.config.memory_budget_mib as usize - state.memory.available_permits(),
        "max_render_pixels": state.config.max_render_pixels,
        "write_timeout_seconds": state.config.write_timeout_seconds,
        "max_connections": state.config.max_connections,
        "timeout_seconds": state.config.timeout_seconds,
        "shutdown_grace_seconds": state.config.shutdown_grace_seconds
    }))
}

async fn shutdown(ExtractState(state): ExtractState<Arc<State>>) -> (StatusCode, Json<Value>) {
    state.stop.send_replace(true);
    (StatusCode::ACCEPTED, Json(json!({"status": "stopping"})))
}

async fn job(
    ExtractState(state): ExtractState<Arc<State>>,
    command: Result<Path<String>, axum::extract::rejection::PathRejection>,
    request: Request,
) -> Result<Response, ApiError> {
    let Path(command) =
        command.map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))?;
    if state.stopping() {
        return Err(ApiError::unavailable("service is stopping"));
    }
    let admission = state
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::unavailable("request capacity exhausted"))?;
    let deadline = Instant::now() + Duration::from_secs(state.config.timeout_seconds);
    let mut args = read_request(request).await?.args;
    if command != "run" {
        args.insert(0, command);
    }
    validate(&args)?;
    let output = worker::submit(state, args, admission, deadline)
        .await
        .map_err(ApiError::internal)??;
    json_file(output.stdout, output.admission).await
}

async fn read_request(request: Request) -> Result<JobRequest, ApiError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    if !content_type.is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
    }) {
        return Err(ApiError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json".into(),
        ));
    }
    let bytes = tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(request.into_body(), 64 * 1024),
    )
    .await
    .map_err(|_| ApiError(StatusCode::REQUEST_TIMEOUT, "request body timeout".into()))?
    .map_err(|error| ApiError(StatusCode::PAYLOAD_TOO_LARGE, error.to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_require_exact_match() {
        assert!(token_matches(b"secret", b"secret"));
        assert!(!token_matches(b"Secret", b"secret"));
        assert!(!token_matches(b"secret!", b"secret"));
    }

    #[tokio::test]
    async fn body_validation_rejects_wrong_type_and_oversize() {
        let request = Request::builder()
            .body(axum::body::Body::empty())
            .expect("request");
        assert_eq!(
            read_request(request).await.err().expect("error").0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        let request = Request::builder()
            .header("content-type", "application/json")
            .body(axum::body::Body::from(vec![b' '; 65537]))
            .expect("request");
        assert_eq!(
            read_request(request).await.err().expect("error").0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let request = Request::builder()
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"args":["inspect","a.pdf"]}"#))
            .expect("request");
        assert_eq!(
            read_request(request).await.expect("valid JSON").args,
            ["inspect", "a.pdf"]
        );
    }
}
