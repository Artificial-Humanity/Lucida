//! A scripted HTTP server for recorded-response tests. Compiled only for tests.
//!
//! Five providers and two of them cost money per render, so "verified by hand
//! against the live API" stopped scaling the moment it started. What the unit
//! tests could not see was the wire: which URL was called, which headers carried
//! the credential, what the body actually said, and whether a multi-step flow
//! (submit, poll, download) followed the URLs the server handed back. Those are
//! exactly the claims this file makes checkable.
//!
//! The shape is deliberate:
//!
//! - **Responses are scripted, not simulated.** A test passes a list of replies
//!   transcribed from real provider responses, and the server plays them back in
//!   order. There is no routing and no cleverness — if the code under test makes
//!   its requests in a different order than the recording, the assertions on the
//!   recorded requests say so.
//! - **The script is a contract, and `finish()` enforces it.** The server keeps
//!   listening until `finish()`, answers any request beyond the script with a
//!   500 that says so, and then fails the test if a request went beyond the
//!   script or a scripted reply was never asked for. It used to stop listening
//!   after the last reply, so an extra request was refused at connect and never
//!   recorded, and `serve(vec![])` never listened at all — which made
//!   "no request reached the API" an assertion that could not fail.
//! - **Requests are recorded whole**: method, path with query, headers, body
//!   bytes. Assertions read what was actually sent rather than what the client
//!   intended to send, which is the difference that catches a credential on the
//!   wrong request.
//! - **No dependency.** A mock-HTTP crate would pull in a runtime for what is a
//!   hundred lines of `TcpListener` — the same reasoning that kept a JSON-RPC
//!   crate out of `mcp.rs`.
//!
//! `{{server}}` in a reply body is replaced with the server's own base URL, so a
//! recording can hand back a polling or download URL that points at the test
//! server — which is how "the client follows the URL the API returned, verbatim"
//! becomes testable.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// One request, as it arrived on the socket.
pub struct Recorded {
    pub method: String,
    /// Path including the query string.
    pub path: String,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    /// A header by case-insensitive name, or None if it was never sent —
    /// which for credentials is sometimes the assertion itself.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("the recorded request body was not JSON")
    }
}

/// One scripted response.
pub struct Reply {
    status: u16,
    content_type: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// What `Content-Length` claims, when that is deliberately not the body's
    /// length — see [`Reply::truncated`].
    declared_length: Option<usize>,
}

impl Reply {
    pub fn json(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "application/json".to_string(),
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
            declared_length: None,
        }
    }

    pub fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json".to_string(),
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
            declared_length: None,
        }
    }

    pub fn bytes(content_type: &str, body: &[u8]) -> Self {
        Self {
            status: 200,
            content_type: content_type.to_string(),
            headers: Vec::new(),
            body: body.to_vec(),
            declared_length: None,
        }
    }

    /// Promises `declared` bytes and sends fewer before closing the connection,
    /// so the client gets its status line and headers and then fails reading
    /// the body — the one failure a download can have after the request itself
    /// has succeeded, which no status code can script.
    pub fn truncated(mut self, declared: usize) -> Self {
        assert!(declared > self.body.len(), "a truncated reply must promise more than it sends");
        self.declared_length = Some(declared);
        self
    }

    /// Adds a response header, transcribed from a real reply — how a recording
    /// carries the `seed` header Stability reports its chosen seed in.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// How long `finish()` waits for the scripted replies to be asked for before it
/// reports them unused — a bound, so a test can never hang the suite.
const DEADLINE: Duration = Duration::from_secs(15);

pub struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    /// How many replies the script holds; requests past this index are extras.
    scripted: usize,
    deadline: Duration,
    stop: Arc<AtomicBool>,
    finished: bool,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Ends the conversation and returns the requests in the order they arrived.
    /// Consuming `self`, because the requests are only complete once the
    /// conversation is over.
    ///
    /// Panics, failing the calling test, when the conversation did not match the
    /// script: a request beyond it (the server answered that one with a 500), or
    /// a scripted reply nobody asked for. A test whose code under test really
    /// makes a variable number of requests must script for that, not tolerate it.
    pub fn finish(mut self) -> Vec<Recorded> {
        self.finished = true;

        // The code under test has normally returned by now, so every request it
        // made is already recorded. The wait covers a request still in flight.
        let deadline = Instant::now() + self.deadline;
        while self.requests.lock().unwrap().len() < self.scripted && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }

        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let requests = std::mem::take(&mut *self.requests.lock().unwrap());

        let seen: Vec<String> = requests.iter().map(|r| format!("{} {}", r.method, r.path)).collect();
        if requests.len() > self.scripted {
            panic!(
                "test server: {} request(s) beyond the script of {} reply(ies), each answered with a 500: {}",
                requests.len() - self.scripted,
                self.scripted,
                seen[self.scripted..].join(", ")
            );
        }
        if requests.len() < self.scripted {
            panic!(
                "test server: {} of {} scripted reply(ies) were never asked for (only saw: [{}])",
                self.scripted - requests.len(),
                self.scripted,
                seen.join(", ")
            );
        }
        requests
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // A server dropped without `finish()` checked nothing: the test never
        // compared the conversation with the script, so it would pass whatever
        // the code under test sent. Not while unwinding — a second panic there
        // aborts the whole test process and hides the first failure.
        if !self.finished && !std::thread::panicking() {
            panic!("test server dropped without finish(): the script was never checked against the requests");
        }
    }
}

/// Starts a server that answers with `replies`, in order, and keeps listening
/// until `finish()` so that a request beyond the script is seen, not refused.
pub fn serve(replies: Vec<Reply>) -> Server {
    serve_with_deadline(replies, DEADLINE)
}

/// `serve` with its own bound on how long `finish()` waits for unused replies,
/// so the server's own tests can watch one go unused without a 15 s wait.
fn serve_with_deadline(replies: Vec<Reply>, deadline: Duration) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binding the test server");
    let url = format!("http://{}", listener.local_addr().unwrap());

    // `{{server}}` resolves now, when the port is known.
    let replies: Vec<Reply> = replies
        .into_iter()
        .map(|mut reply| {
            if let Ok(text) = std::str::from_utf8(&reply.body) {
                if text.contains("{{server}}") {
                    reply.body = text.replace("{{server}}", &url).into_bytes();
                }
            }
            reply
        })
        .collect();
    let scripted = replies.len();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);

    let handle = std::thread::spawn(move || {
        // Non-blocking accept, so the thread can notice `stop` rather than sit
        // in `accept` forever once the test is over.
        listener.set_nonblocking(true).ok();
        let unexpected = Reply::status(
            500,
            r#"{"error":"test server: unexpected request, the script had no reply left"}"#,
        );

        while !stopping.load(Ordering::SeqCst) {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(_) => break,
            };
            stream.set_nonblocking(false).ok();
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            // Connections are handled one at a time, so the next reply is the
            // one after however many requests have been recorded.
            let next = recorded.lock().unwrap().len();
            let reply = replies.get(next).unwrap_or(&unexpected);
            if let Err(e) = handle_connection(stream, reply, &recorded) {
                eprintln!("test server: {e}");
            }
        }
    });

    Server {
        url,
        requests,
        scripted,
        deadline,
        stop,
        finished: false,
        handle: Some(handle),
    }
}

fn handle_connection(
    stream: TcpStream,
    reply: &Reply,
    recorded: &Arc<Mutex<Vec<Recorded>>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    if request_line.trim().is_empty() {
        // A connection that closed without sending a request is not one.
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    // Bodies arrive either sized or chunked — reqwest sizes in-memory bodies and
    // chunks streamed ones, and which a multipart form gets is an implementation
    // detail not worth depending on, so both are handled.
    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok());
    let chunked = headers
        .iter()
        .any(|(name, value)| name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked"));

    let mut body = Vec::new();
    if let Some(length) = content_length {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else if chunked {
        loop {
            let mut size_line = String::new();
            reader.read_line(&mut size_line)?;
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            if size == 0 {
                let mut trailer = String::new();
                reader.read_line(&mut trailer)?;
                break;
            }
            let mut chunk = vec![0u8; size];
            reader.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf)?;
        }
    }

    recorded.lock().unwrap().push(Recorded {
        method,
        path,
        headers,
        body,
    });

    let mut stream = reader.into_inner();
    let mut head = format!(
        "HTTP/1.1 {} recorded\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.content_type,
        reply.declared_length.unwrap_or(reply.body.len())
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    /// One bare GET, returning the status line — no HTTP client, so these tests
    /// exercise the server and nothing else.
    fn get(server: &Server, path: &str) -> String {
        let address = server.url().trim_start_matches("http://");
        let mut stream = TcpStream::connect(address).expect("connecting to the test server");
        write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response.lines().next().unwrap_or_default().to_string()
    }

    fn panic_message(result: std::thread::Result<Vec<Recorded>>) -> String {
        let payload = result.err().expect("finish() should have panicked");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    }

    #[test]
    fn a_conversation_that_matches_the_script_passes() {
        let server = serve(vec![Reply::json("{}")]);
        assert!(get(&server, "/one").contains("200"));
        let requests = server.finish();
        assert_eq!(requests[0].path, "/one");
    }

    /// The defect this guards: with an empty script the listener used to drop at
    /// once, so a request was refused at connect and never recorded.
    #[test]
    fn a_request_to_an_empty_script_is_recorded_answered_500_and_fails_the_test() {
        let server = serve(vec![]);
        assert!(get(&server, "/sneaked").contains("500"));
        let message = panic_message(catch_unwind(AssertUnwindSafe(|| server.finish())));
        assert!(message.contains("beyond the script"), "{message}");
        assert!(message.contains("GET /sneaked"), "{message}");
    }

    /// The listener used to close after the last reply, so a second request was
    /// refused and went unrecorded.
    #[test]
    fn a_request_past_the_last_reply_is_recorded_and_fails_the_test() {
        let server = serve(vec![Reply::json("{}")]);
        assert!(get(&server, "/first").contains("200"));
        assert!(get(&server, "/second").contains("500"));
        let message = panic_message(catch_unwind(AssertUnwindSafe(|| server.finish())));
        assert!(message.contains("1 request(s) beyond the script of 1"), "{message}");
        assert!(message.contains("GET /second"), "{message}");
    }

    #[test]
    fn a_scripted_reply_nobody_asked_for_fails_the_test() {
        let server = serve_with_deadline(vec![Reply::json("{}"), Reply::json("{}")], Duration::from_millis(300));
        get(&server, "/only");
        let message = panic_message(catch_unwind(AssertUnwindSafe(|| server.finish())));
        assert!(message.contains("1 of 2 scripted reply(ies) were never asked for"), "{message}");
        assert!(message.contains("GET /only"), "{message}");
    }

    #[test]
    fn a_server_dropped_without_finish_fails_the_test() {
        let server = serve(vec![]);
        let result = catch_unwind(AssertUnwindSafe(|| drop(server)));
        assert!(result.is_err(), "dropping an unchecked server must panic");
    }
}
