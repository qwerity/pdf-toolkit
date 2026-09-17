use std::{
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    body::{Body, Bytes},
    http::header,
    response::{IntoResponse, Response},
};
use futures_core::Stream;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
    sync::OwnedSemaphorePermit,
};
use tokio_util::io::ReaderStream;

use super::error::ApiError;

/// Keep admission capacity until delivery ends, bounding slow-client buffers/FDs too.
struct ResponseStream {
    reader: ReaderStream<File>,
    _admission: OwnedSemaphorePermit,
}

impl Stream for ResponseStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.reader).poll_next(cx)
    }
}

pub async fn json_file(
    mut file: File,
    admission: OwnedSemaphorePermit,
) -> Result<Response, ApiError> {
    let length = file.metadata().await.map_err(ApiError::internal)?.len();
    if length == 0 {
        return Err(ApiError::internal("worker returned empty JSON output"));
    }
    file.rewind().await.map_err(ApiError::internal)?;
    let stream = ResponseStream {
        reader: ReaderStream::with_capacity(file, 16 * 1024),
        _admission: admission,
    };
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::CONTENT_LENGTH, length.to_string()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

/// Return bounded diagnostics even when the job produced a very large stderr log.
pub async fn error_excerpt(mut file: File) -> Result<String, ApiError> {
    file.rewind().await.map_err(ApiError::internal)?;
    let mut bytes = Vec::new();
    file.take(8192)
        .read_to_end(&mut bytes)
        .await
        .map_err(ApiError::internal)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, sync::Arc};
    use tokio::sync::Semaphore;

    #[tokio::test]
    async fn response_streams_exact_json_and_releases_admission() {
        let mut file = tempfile::tempfile().expect("temporary file");
        file.write_all(b"{\"pages\":{}}\n").expect("write JSON");
        let slots = Arc::new(Semaphore::new(1));
        let response = json_file(
            File::from_std(file),
            slots.clone().acquire_owned().await.expect("permit"),
        )
        .await
        .expect("response");
        assert_eq!(slots.available_permits(), 0);
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("body");
        assert_eq!(&bytes[..], b"{\"pages\":{}}\n");
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn diagnostics_are_bounded_and_empty_success_is_rejected() {
        let mut file = tempfile::tempfile().expect("file");
        file.write_all(&vec![b'x'; 9000]).expect("write");
        assert_eq!(
            error_excerpt(File::from_std(file))
                .await
                .expect("excerpt")
                .len(),
            8192
        );
        let slots = Arc::new(Semaphore::new(1));
        assert!(
            json_file(
                File::from_std(tempfile::tempfile().expect("file")),
                slots.acquire_owned().await.expect("permit")
            )
            .await
            .is_err()
        );
    }
}
