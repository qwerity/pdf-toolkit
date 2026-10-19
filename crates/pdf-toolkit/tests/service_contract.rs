//! Service compatibility and lifecycle checks against the actual executable.
mod common;
#[path = "service/support.rs"]
mod support;

use serde_json::json;
use std::process::Command;
use support::Server;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("fixture directory");
    let input = dir.path().join("input.pdf");
    common::write_simple_pdf(&common::pdfium(), &input, 2);
    (dir, input)
}

#[test]
fn defaults_health_authentication_and_shutdown() {
    let mut server = Server::start(&[], Some("test-token"));
    assert_eq!(server.call("GET", "/health", None, None).0, 401);
    assert_eq!(
        server.call("GET", "/health", None, Some("test-token")).0,
        200
    );
    assert_eq!(
        server.call("GET", "/ready", None, Some("test-token")).0,
        200
    );
    let (status, body) = server.call("GET", "/status", None, Some("test-token"));
    assert_eq!(status, 200);
    assert_eq!(body["max_workers"], 8);
    assert_eq!(body["active_workers"], 0);
    assert_eq!(
        server.call("POST", "/shutdown", None, Some("test-token")).0,
        202
    );
    server.wait_for_exit();
}

#[test]
fn open_connection_limit_preserves_existing_connections() {
    use std::{io::Read, io::Write, net::TcpStream, time::Duration};

    let server = Server::start(&["--max-connections", "1"], None);
    let mut kept = TcpStream::connect(server.address).expect("connection");
    kept.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let request = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
    kept.write_all(request).expect("health request");
    read_health_response(&mut kept);

    let mut excess = TcpStream::connect(server.address).expect("excess connection");
    excess
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let result = excess.read(&mut [0]);
    assert!(
        matches!(result, Ok(0))
            || matches!(result, Err(ref error) if matches!(error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted)),
        "excess connection must close: {result:?}"
    );
    kept.write_all(request).expect("existing connection usable");
    read_health_response(&mut kept);
}

fn read_health_response(stream: &mut std::net::TcpStream) {
    use std::io::Read;
    let mut response = Vec::new();
    while !response.ends_with(br#"{"status":"ok"}"#) {
        let mut byte = [0];
        stream.read_exact(&mut byte).expect("health response");
        response.push(byte[0]);
        assert!(response.len() < 4096, "bounded health response");
    }
    assert!(response.starts_with(b"HTTP/1.1 200"));
}

#[test]
fn sixteen_callers_get_cli_identical_json_with_no_idle_workers() {
    let (_dir, input) = fixture();
    let server = Server::start(&["--max-workers", "8"], None);
    let expected = Command::new(env!("CARGO_BIN_EXE_pdf-toolkit"))
        .arg("inspect")
        .arg(&input)
        .current_dir(common::workspace_root())
        .output()
        .expect("CLI output");
    assert!(expected.status.success());
    let expected: serde_json::Value = serde_json::from_slice(&expected.stdout).expect("CLI JSON");
    std::thread::scope(|scope| {
        let mut jobs = Vec::new();
        for _ in 0..16 {
            let input = &input;
            let expected = &expected;
            let server = &server;
            jobs.push(scope.spawn(move || {
                let (status, body) =
                    server.call("POST", "/v1/inspect", Some(json!({"args": [input]})), None);
                assert_eq!(status, 200, "{body}");
                assert_eq!(&body, expected);
            }));
        }
        for job in jobs {
            job.join().expect("concurrent request");
        }
    });
    let (_, status) = server.call("GET", "/status", None, None);
    assert_eq!(status["active_workers"], 0);
    assert_eq!(status["outstanding_requests"], 0);
}

#[test]
fn render_and_text_outputs_survive_the_worker() {
    let (dir, input) = fixture();
    let server = Server::start(&["--max-workers", "2"], None);
    assert_eq!(
        server.call("GET", "/status", None, None).1["max_workers"],
        2
    );
    for command in ["render", "text"] {
        let (status, body) = server.call(
            "POST",
            "/v1/run",
            Some(json!({"args": [command, input, "--pages", "1", "--output", dir.path()]})),
            None,
        );
        assert_eq!(status, 200, "{body}");
        let output = body["pages"]["1"]["path"].as_str().expect("output path");
        assert!(std::fs::metadata(output).expect("persisted artifact").len() > 0);
    }
}

#[test]
fn invalid_jobs_return_json_without_crashing_service() {
    let server = Server::start(&[], None);
    for (args, expected) in [
        (json!(["serve"]), 400),
        (json!(["--help"]), 400),
        (json!(["render", "missing.pdf"]), 400),
        (json!(["inspect", "missing.pdf"]), 422),
    ] {
        let (status, body) = server.call("POST", "/v1/run", Some(json!({"args": args})), None);
        assert_eq!(status, expected, "{body}");
        assert!(body["error"]["message"].is_string());
    }
    assert_eq!(server.call("GET", "/health", None, None).0, 200);
}

#[cfg(unix)]
#[test]
fn disconnected_parent_pipe_terminates_worker() {
    // Service children use this same entry-point monitor. Run a long-lived
    // mode so the check cannot race completion of a tiny PDF operation.
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdf-toolkit"))
        .args(["serve", "--bind", "127.0.0.1:0"])
        .env("PDF_TOOLKIT_PARENT_PIPE", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("monitored process");
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            assert_eq!(status.code(), Some(125));
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("parent EOF did not terminate worker");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn full_admission_returns_503_while_health_stays_responsive() {
    use std::io::Write;
    let server = Server::start(&["--max-workers", "1", "--max-queue", "0"], None);
    let mut waiting = std::net::TcpStream::connect(server.address).expect("connect");
    write!(waiting, "POST /v1/run HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n").expect("headers");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while server.call("GET", "/status", None, None).1["outstanding_requests"] != 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "request was not admitted"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (status, _) = server.call(
        "POST",
        "/v1/inspect",
        Some(json!({"args": ["missing.pdf"]})),
        None,
    );
    assert_eq!(status, 503);
    assert_eq!(server.call("GET", "/health", None, None).0, 200);
    drop(waiting);
}

#[test]
fn expanded_pages_and_http_errors_are_bounded_json_rejections() {
    let server = Server::start(&[], None);
    let pages = vec!["1-65535"; 256].join(",");
    let (status, body) = server.call(
        "POST",
        "/v1/structured",
        Some(json!({"args": ["missing.pdf", "--pages", pages]})),
        None,
    );
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error")
            .contains("65535")
    );
    for (method, path, expected) in [("GET", "/v1/run", 405), ("POST", "/v1/%FF", 400)] {
        let (status, body) = server.call(method, path, Some(json!({"args": []})), None);
        assert_eq!(status, expected);
        assert!(body["error"]["message"].is_string());
    }
    assert_eq!(
        server.call("GET", "/status", None, None).1["active_workers"],
        0
    );
}

#[test]
fn rendering_rejects_oversized_bitmaps_before_allocation() {
    let (dir, input) = fixture();
    let server = Server::start(&["--max-render-pixels", "1"], None);
    let (status, body) = server.call(
        "POST",
        "/v1/render",
        Some(json!({"args": [input, "--pages", "1", "--output", dir.path()]})),
        None,
    );
    assert_eq!(status, 422, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error")
            .contains("render dimensions")
    );
}

#[test]
fn stalled_response_reader_releases_capacity_without_disconnecting() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    // A response large enough to outlast a 1 KiB receive window.
    let pdf = common::data_dir().join("plan/3.pdf");
    if !common::corpus_available(
        "stalled_response_reader_releases_capacity_without_disconnecting",
        std::slice::from_ref(&pdf),
    ) {
        return;
    }
    let server = Server::start(
        &[
            "--max-workers",
            "1",
            "--max-queue",
            "0",
            "--write-timeout-seconds",
            "1",
        ],
        None,
    );
    let mut client = std::net::TcpStream::connect(server.address).expect("connect");
    socket2::SockRef::from(&client)
        .set_recv_buffer_size(1024)
        .expect("small receive window");
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    let body = json!({"args": [pdf]}).to_string();
    write!(client, "POST /v1/vector HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).expect("request");
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        client.read_exact(&mut byte).expect("response header");
        headers.push(byte[0]);
        assert!(headers.len() < 4096);
    }
    assert!(String::from_utf8_lossy(&headers).starts_with("HTTP/1.1 200"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.call("GET", "/status", None, None).1["outstanding_requests"] != 0 {
        assert!(
            Instant::now() < deadline,
            "stalled reader retained admission"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        server
            .call(
                "POST",
                "/v1/inspect",
                Some(json!({"args": ["missing.pdf"]})),
                None
            )
            .0,
        422
    );
    // `client` remains open throughout: capacity must be released by the server.
    drop(client);
}

#[cfg(unix)]
#[test]
fn excessive_worker_memory_is_terminated_and_releases_reservation() {
    let (dir, input) = fixture();
    let output = dir.path().join("blocked.pdf");
    assert!(
        Command::new("mkfifo")
            .arg(&output)
            .status()
            .expect("FIFO")
            .success()
    );
    let server = Server::start(
        &[
            "--worker-memory-mib",
            "1",
            "--memory-budget-mib",
            "1",
            "--timeout-seconds",
            "10",
        ],
        None,
    );
    let (status, body) = server.call(
        "POST",
        "/v1/downsample",
        Some(json!({"args": [input, "-o", output]})),
        None,
    );
    assert_eq!(status, 422, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error")
            .contains("memory limit")
    );
    let (_, status) = server.call("GET", "/status", None, None);
    assert_eq!(status["active_workers"], 0);
    assert_eq!(status["reserved_memory_mib"], 0);
}
