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
    body: Vec<u8>,
}

impl Reply {
    pub fn json(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "application/json".to_string(),
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json".to_string(),
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn bytes(content_type: &str, body: &[u8]) -> Self {
        Self {
            status: 200,
            content_type: content_type.to_string(),
            body: body.to_vec(),
        }
    }
}

pub struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Waits for every scripted reply to be consumed and returns the requests in
    /// the order they arrived. Consuming `self`, because the requests are only
    /// complete once the conversation is over.
    pub fn finish(mut self) -> Vec<Recorded> {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

/// Starts a server that answers with `replies`, in order, then stops listening.
pub fn serve(replies: Vec<Reply>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binding the test server");
    let url = format!("http://{}", listener.local_addr().unwrap());

    // `{{server}}` resolves now, when the port is known.
    let replies: Vec<Reply> = replies
        .into_iter()
        .map(|mut reply| {
            if let Ok(text) = std::str::from_utf8(&reply.body)
                && text.contains("{{server}}")
            {
                reply.body = text.replace("{{server}}", &url).into_bytes();
            }
            reply
        })
        .collect();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);

    let handle = std::thread::spawn(move || {
        // Non-blocking accept with a deadline, so a test whose code makes fewer
        // requests than the script expects fails in seconds rather than hanging
        // the suite on a join that can never complete.
        listener.set_nonblocking(true).ok();
        let deadline = Instant::now() + Duration::from_secs(15);

        'replies: for reply in replies {
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() > deadline {
                            eprintln!("test server: gave up waiting for a request");
                            break 'replies;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break 'replies,
                }
            };
            stream.set_nonblocking(false).ok();
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            if let Err(e) = handle_connection(stream, &reply, &recorded) {
                eprintln!("test server: {e}");
                break;
            }
        }
    });

    Server {
        url,
        requests,
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
    let head = format!(
        "HTTP/1.1 {} recorded\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.status,
        reply.content_type,
        reply.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}
