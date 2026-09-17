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
