use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use serde_json::Value;

pub struct Server {
    pub child: Child,
    pub address: SocketAddr,
}

impl Server {
    pub fn start(args: &[&str], token: Option<&str>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pdf-toolkit"));
        command
            .args(["serve", "--bind", "127.0.0.1:0"])
            .args(args)
            .current_dir(crate::common::workspace_root())
            .env_remove("PDF_TOOLKIT_PARENT_PIPE")
            .env_remove("PDF_TOOLKIT_SERVICE_TOKEN")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(token) = token {
            command.env("PDF_TOOLKIT_SERVICE_TOKEN", token);
        }
        let mut child = command.spawn().expect("start service");
        let stderr = child.stderr.take().expect("stderr pipe");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if sender.send(line.expect("startup line")).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            address: "127.0.0.1:0".parse().expect("address"),
        };
        let line = receiver
            .recv_timeout(Duration::from_secs(15))
            .expect("service startup");
        server.address = line
            .strip_prefix("pdf-toolkit service listening on ")
            .and_then(|rest| rest.split_whitespace().next())
            .expect("startup address")
            .parse()
            .expect("socket address");
        server
    }

    pub fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (u16, Value) {
        call(self.address, method, path, body, token)
    }

    pub fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                assert!(status.success(), "service exit: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "service failed to stop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn call(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (u16, Value) {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("write timeout");
    let body = body.map(|body| body.to_string()).unwrap_or_default();
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len()).expect("request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    let (headers, body) = response.split_once("\r\n\r\n").expect("HTTP response");
    let status = headers
        .split_whitespace()
        .nth(1)
        .expect("status")
        .parse()
        .expect("status number");
    (
        status,
        serde_json::from_str(body)
            .unwrap_or_else(|error| panic!("invalid JSON: {error}: {response}")),
    )
}
