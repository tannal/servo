/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A deliberately small blocking HTTP client used by the `servo:dom/fetch`
//! host interface.  Requests run on a dedicated thread owned by the caller so
//! that long LLM generations never block Servo's ScriptThread.
//!
//! The first version only speaks `http://`.  This is enough for local model
//! servers such as Ollama (`http://127.0.0.1:11434`) and keeps the trusted
//! computing base small.  HTTPS can be added later behind the same interface
//! without touching the guest ABI.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use url::Url;

/// The maximum response body we are willing to buffer.  Chat completions and
/// attached images are comfortably below this; anything larger is treated as
/// an error instead of exhausting the browser process.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Perform a single HTTP/1.1 request.  Header names are matched
/// case-insensitively; the body is either sent as-is or omitted.
pub fn fetch_blocking(
    url: &str,
    method: &str,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
) -> Result<HttpResponse, String> {
    let parsed = Url::parse(url).map_err(|error| format!("invalid URL: {error}"))?;
    if parsed.scheme() == "https" {
        return fetch_https(url, method, headers, body);
    }
    if parsed.scheme() != "http" {
        return Err(format!(
            "unsupported URL scheme '{}': only http:// and https:// are supported",
            parsed.scheme()
        ));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let path = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };

    let mut last_error = None;
    let mut stream = None;
    for address in (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("could not resolve {host}:{port}: {error}"))?
    {
        match TcpStream::connect_timeout(&address, Duration::from_secs(10)) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            },
            Err(error) => last_error = Some(error),
        }
    }
    let mut stream = stream.ok_or_else(|| {
        format!(
            "could not connect to {host}:{port}: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "no addresses".to_string())
        )
    })?;

    // Local models can take minutes to produce a full response.
    stream
        .set_read_timeout(Some(Duration::from_secs(300)))
        .map_err(|error| format!("could not set read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| format!("could not set write timeout: {error}"))?;

    let body = body.unwrap_or_default();
    let mut request = String::with_capacity(512);
    request.push_str(method);
    request.push(' ');
    request.push_str(&path);
    request.push_str(" HTTP/1.1\r\n");
    request.push_str("Host: ");
    request.push_str(host);
    if !matches!((parsed.scheme(), port), ("http", 80)) {
        request.push(':');
        request.push_str(&port.to_string());
    }
    request.push_str("\r\nConnection: close\r\nUser-Agent: Servo-Wasm/0.1\r\nAccept: */*\r\n");
    for (name, value) in &headers {
        // Reject header injection outright instead of trying to sanitize it.
        if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
            return Err("invalid header: contains CR or LF".to_string());
        }
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    if !body.is_empty() || !matches!(method, "GET" | "HEAD") {
        request.push_str("Content-Length: ");
        request.push_str(&body.len().to_string());
        request.push_str("\r\n");
    }
    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(&body))
        .and_then(|_| stream.flush())
        .map_err(|error| format!("could not send request: {error}"))?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|error| format!("could not read status line: {error}"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed status line: {status_line:?}"))?
        .parse::<u16>()
        .map_err(|_| format!("malformed status code: {status_line:?}"))?;

    let mut content_length = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| format!("could not read response headers: {error}"))?;
        if read == 0 {
            return Err("connection closed before response headers completed".to_string());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .split(',')
                .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"));
        }
    }

    let body = if chunked {
        read_chunked(&mut reader)?
    } else if let Some(length) = content_length {
        if length > MAX_BODY_BYTES {
            return Err(format!("response body of {length} bytes is too large"));
        }
        let mut body = vec![0; length];
        reader
            .read_exact(&mut body)
            .map_err(|error| format!("could not read response body: {error}"))?;
        body
    } else {
        let mut body = Vec::new();
        reader
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| format!("could not read response body: {error}"))?;
        if body.len() > MAX_BODY_BYTES {
            return Err("response body is too large".to_string());
        }
        body
    };

    Ok(HttpResponse { status, body })
}

fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .map_err(|error| format!("could not read chunk size: {error}"))?;
        let size_token = size_line
            .trim()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim();
        let size = usize::from_str_radix(size_token, 16)
            .map_err(|_| format!("malformed chunk size: {size_line:?}"))?;
        if size == 0 {
            // Consume optional trailers.
            loop {
                let mut trailer = String::new();
                let read = reader
                    .read_line(&mut trailer)
                    .map_err(|error| format!("could not read trailers: {error}"))?;
                if read == 0 || trailer.trim().is_empty() {
                    break;
                }
            }
            break;
        }
        if body.len().saturating_add(size) > MAX_BODY_BYTES {
            return Err("chunked response body is too large".to_string());
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader
            .read_exact(&mut body[start..])
            .map_err(|error| format!("could not read chunk: {error}"))?;
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .map_err(|error| format!("could not read chunk terminator: {error}"))?;
    }
    Ok(body)
}


/// HTTPS requests are delegated to `ureq`, which handles TLS roots and
/// redirects.  The browser's own network stack is still the long-term home for
/// this capability; this keeps the experimental component runtime usable with
/// hosted OpenAI-compatible endpoints.
fn fetch_https(
    url: &str,
    method: &str,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
) -> Result<HttpResponse, String> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(300))
        .timeout_write(Duration::from_secs(30))
        .redirects(5);

    // Respect the conventional proxy variables for HTTPS only.  Plain HTTP is
    // used for local model servers and deliberately bypasses proxies.
    for key in ["SERVO_WASM_HTTPS_PROXY", "HTTPS_PROXY", "https_proxy"] {
        if let Ok(proxy) = std::env::var(key) {
            if !proxy.is_empty() {
                let proxy = ureq::Proxy::new(&proxy)
                    .map_err(|error| format!("invalid HTTPS proxy: {error}"))?;
                builder = builder.proxy(proxy);
                break;
            }
        }
    }

    let agent = builder.build();
    let mut request = agent.request(method, url);
    for (name, value) in headers {
        request = request.set(&name, &value);
    }
    let response = match body {
        Some(body) if !body.is_empty() => request.send_bytes(&body),
        _ => request.call(),
    };
    let (status, response) = match response {
        Ok(response) => (response.status(), response),
        Err(ureq::Error::Status(status, response)) => (status, response),
        Err(ureq::Error::Transport(error)) => {
            return Err(format!("HTTPS request failed: {error}"));
        },
    };

    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| format!("could not read response body: {error}"))?;
    if body.len() > MAX_BODY_BYTES {
        return Err("response body is too large".to_string());
    }
    Ok(HttpResponse { status, body })
}
