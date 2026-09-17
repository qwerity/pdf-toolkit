use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub fn unavailable(message: &str) -> Self {
        Self(StatusCode::SERVICE_UNAVAILABLE, message.into())
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }

    pub fn timeout() -> Self {
        Self(
            StatusCode::GATEWAY_TIMEOUT,
            "request deadline exceeded".into(),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.0, Json(json!({"error": {"message": self.1}}))).into_response();
        if self.0 == StatusCode::SERVICE_UNAVAILABLE {
            response
                .headers_mut()
                .insert("retry-after", "1".parse().expect("literal header"));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_errors_are_retryable_json() {
        let response = ApiError::unavailable("queue full").into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["retry-after"], "1");
        assert_eq!(response.headers()["content-type"], "application/json");
        assert_eq!(ApiError::timeout().0, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            ApiError::internal("failure").0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
