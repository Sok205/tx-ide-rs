//! The `http` check: a minimal HTTP/1.1 client, the response matcher, and the pack server's
//! lifecycle (free port via `PORT`, readiness with backoff, stderr tail, kill on drop).

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::tutor::pack::HttpCheck;

pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const TAIL_LINES: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub fn parse_response(raw: &str) -> io::Result<Response> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not an HTTP response");
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(invalid)?;
    let mut parts = status_line.split_whitespace();
    if !parts.next().is_some_and(|version| version.starts_with("HTTP/")) {
        return Err(invalid());
    }
    let status = parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(invalid)?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect();
    Ok(Response {
        status,
        headers,
        body: body.to_owned(),
    })
}

pub fn request(port: u16, method: &str, path: &str, body: Option<&str>) -> io::Result<Response> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let body = body.unwrap_or("");
    let mut message = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
        message.push_str("Content-Type: application/json\r\n");
    }
    message.push_str("\r\n");
    message.push_str(body);
    stream.write_all(message.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    parse_response(&String::from_utf8_lossy(&raw))
}

/// `*` matches any run of characters; everything else matches itself, ASCII case-insensitive.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();
    let mut pieces = pattern.split('*');
    let first = pieces.next().unwrap_or("");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let pieces: Vec<&str> = pieces.collect();
    let Some((last, middle)) = pieces.split_last() else {
        return rest.is_empty();
    };
    for piece in middle {
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

pub fn evaluate(check: &HttpCheck, response: &Response) -> Result<(), String> {
    if response.status != check.status {
        return Err(format!(
            "expected status {}, got {}",
            check.status, response.status
        ));
    }
    for (name, pattern) in &check.headers {
        match response.header(name) {
            None => return Err(format!("missing header {name}")),
            Some(value) if !glob_match(pattern, value) => {
                return Err(format!(
                    "header {name}: expected '{pattern}', got '{value}'"
                ));
            }
            Some(_) => {}
        }
    }
    if let Some(needle) = &check.contains
        && !response.body.contains(needle.as_str())
    {
        let preview: String = response.body.chars().take(200).collect();
        return Err(format!("body does not contain '{needle}'; got: {preview}"));
    }
    if check.json && serde_json::from_str::<serde_json::Value>(&response.body).is_err() {
        return Err("body is not valid JSON".to_owned());
    }
    Ok(())
}

/// The pack's server, running in the project on a free port; killed on drop.
pub struct Server {
    child: Child,
    pub port: u16,
    stderr_path: PathBuf,
    ready_timeout: Duration,
    _scratch: tempfile::TempDir,
}

impl Server {
    /// `ready_timeout`: `None` defaults to 5 s (`DEFAULT_READY_TIMEOUT`).
    pub fn start(run: &[String], cwd: &Path, ready_timeout: Option<Duration>) -> Result<Self, String> {
        let (program, args) = run
            .split_first()
            .ok_or("pack.toml `run` is empty")?;
        let port = TcpListener::bind(("127.0.0.1", 0))
            .and_then(|listener| listener.local_addr())
            .map_err(|error| format!("could not pick a free port: {error}"))?
            .port();
        let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
        let stderr_path = scratch.path().join("stderr.log");
        let stderr = File::create(&stderr_path).map_err(|error| error.to_string())?;
        let child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env("PORT", port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            // Its own process group, so the server's own children (e.g. a shell's grandchildren
            // spawning the real listener) are killed with it on drop instead of being leaked.
            .process_group(0)
            .spawn()
            .map_err(|error| format!("could not run {program}: {error}"))?;
        let mut server = Self {
            child,
            port,
            stderr_path,
            ready_timeout: ready_timeout.unwrap_or(DEFAULT_READY_TIMEOUT),
            _scratch: scratch,
        };
        server.wait_ready()?;
        Ok(server)
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + self.ready_timeout;
        let mut delay = Duration::from_millis(25);
        loop {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return Ok(());
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "the server exited ({status}) before accepting connections\n{}",
                    self.stderr_tail()
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the server did not listen on port {} within {}s\n{}",
                    self.port,
                    self.ready_timeout.as_secs(),
                    self.stderr_tail()
                ));
            }
            std::thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_millis(400));
        }
    }

    pub fn stderr_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.stderr_path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Kill the whole process group (the server's own children with it); `child.kill()` as a
        // fallback if the group is already gone or the kill otherwise fails.
        let pid = self.child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::process::Command;

    use super::*;

    const RAW: &str = "HTTP/1.0 404 Not Found\r\nServer: x\r\nContent-Type: application/json; charset=utf-8\r\n\r\n{\"error\": \"nope\"}";

    fn check(toml: &str) -> HttpCheck {
        toml::from_str(&format!("label = \"l\"\npath = \"/\"\n{toml}")).unwrap()
    }

    #[test]
    fn parses_status_headers_body() {
        let response = parse_response(RAW).unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.header("content-type"), Some("application/json; charset=utf-8"));
        assert_eq!(response.body, "{\"error\": \"nope\"}");
        assert!(parse_response("garbage").is_err());
    }

    #[test]
    fn globs_are_star_only_and_case_insensitive() {
        assert!(glob_match("application/json*", "Application/JSON; charset=utf-8"));
        assert!(glob_match("*json*", "application/json"));
        assert!(glob_match("text/plain", "text/plain"));
        assert!(!glob_match("text/plain", "text/plain; x"));
        assert!(!glob_match("application/json*", "text/html"));
    }

    #[test]
    fn evaluate_reports_the_first_mismatch() {
        let response = parse_response(RAW).unwrap();
        assert_eq!(evaluate(&check("status = 404\njson = true"), &response), Ok(()));
        assert_eq!(
            evaluate(&check("status = 200"), &response),
            Err("expected status 200, got 404".into())
        );
        assert_eq!(
            evaluate(&check("status = 404\nheaders = { \"Content-Type\" = \"text/*\" }"), &response),
            Err("header Content-Type: expected 'text/*', got 'application/json; charset=utf-8'".into())
        );
        assert_eq!(
            evaluate(&check("status = 404\nheaders = { \"X-Nope\" = \"*\" }"), &response),
            Err("missing header X-Nope".into())
        );
        let error = evaluate(&check("status = 404\ncontains = \"milk\""), &response).unwrap_err();
        assert!(error.starts_with("body does not contain 'milk'"), "{error}");
        let plain = parse_response("HTTP/1.0 200 OK\r\n\r\nhi").unwrap();
        assert_eq!(evaluate(&check("status = 200\njson = true"), &plain), Err("body is not valid JSON".into()));
    }

    #[test]
    fn request_speaks_http_over_tcp() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut line = String::new();
            while line != "\r\n" {
                line.clear();
                reader.read_line(&mut line).unwrap();
            }
            let mut body = [0u8; 2];
            reader.read_exact(&mut body).unwrap();
            stream.write_all(b"HTTP/1.0 201 Created\r\nContent-Type: text/plain\r\n\r\nmade").unwrap();
            request_line
        });
        let response = request(port, "POST", "/notes", Some("{}")).unwrap();
        assert_eq!((response.status, response.body.as_str()), (201, "made"));
        assert_eq!(server.join().unwrap(), "POST /notes HTTP/1.1\r\n");
    }

    #[test]
    fn a_server_that_dies_reports_its_stderr() {
        let run = ["sh".into(), "-c".into(), "echo boom >&2; exit 3".into()];
        let error = Server::start(&run, Path::new("."), None).err().unwrap();
        assert!(error.contains("exited") && error.contains("boom"), "{error}");
    }

    #[test]
    fn a_server_gets_its_port_from_the_environment() {
        if Command::new("python3").arg("--version").output().is_err() {
            eprintln!("skipped: python3 not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "tutor").unwrap();
        let run = [
            "sh".into(),
            "-c".into(),
            "exec python3 -m http.server --bind 127.0.0.1 \"$PORT\"".into(),
        ];
        let server = Server::start(&run, dir.path(), None).unwrap();
        let response = request(server.port, "GET", "/index.html", None).unwrap();
        assert_eq!((response.status, response.body.as_str()), (200, "tutor"));
    }

    #[test]
    fn dropping_the_server_kills_its_process_group() {
        if Command::new("python3").arg("--version").output().is_err() {
            eprintln!("skipped: python3 not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pid");
        let run = [
            "sh".into(),
            "-c".into(),
            format!(
                "sleep 60 & echo $! > '{}' ; exec python3 -m http.server --bind 127.0.0.1 \"$PORT\"",
                pid_path.display()
            ),
        ];
        let server = Server::start(&run, dir.path(), None).unwrap();
        let grandchild_pid: i32 = std::fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive = || unsafe { libc::kill(grandchild_pid, 0) == 0 };
        assert!(alive(), "the background sleep should still be alive before drop");
        drop(server);
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(), "the whole process group should be dead within 2s of drop");
    }
}
