//! High-level HTTPS client.  Performs DNS resolution, TCP connect, TLS 1.3
//! handshake, HTTP/1.1 GET, and returns a parsed response.
//!
//! Phase 1: certificate is not validated.  The browser shows a warning.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use atom_syscall::ipc::PortId;
use libnet::{net_close, net_connect, net_resolve, net_socket, NetError};

use crate::handshake::{run_handshake, HandshakeError};
use crate::record::{RecordError, RecordStream, CT_APPLICATION_DATA};

#[derive(Debug)]
pub enum HttpsError {
    Net(NetError),
    Tls(HandshakeError),
    Record(RecordError),
    EmptyResponse,
    ResponseTooLarge,
}
impl From<NetError> for HttpsError {
    fn from(e: NetError) -> Self {
        Self::Net(e)
    }
}
impl From<HandshakeError> for HttpsError {
    fn from(e: HandshakeError) -> Self {
        Self::Tls(e)
    }
}
impl From<RecordError> for HttpsError {
    fn from(e: RecordError) -> Self {
        Self::Record(e)
    }
}

pub struct HttpsResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub location: Option<String>,
    /// True when the server certificate was NOT validated.
    pub cert_unverified: bool,
}

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Perform an HTTPS GET and return the response.
/// Follows no redirects — caller is responsible for redirect handling.
pub fn https_get(
    netd_port: PortId,
    host: &str,
    path: &str,
    port: u16,
) -> Result<HttpsResponse, HttpsError> {
    // 1. DNS
    let ip = net_resolve(netd_port, host)?;

    // 2. TCP connect
    let socket_id = net_socket(netd_port)?;
    if let Err(e) = net_connect(netd_port, socket_id, ip, port) {
        let _ = net_close(netd_port, socket_id);
        return Err(HttpsError::Net(e));
    }

    // 3. TLS handshake
    let mut stream = RecordStream::new(netd_port, socket_id);
    let hs = match run_handshake(&mut stream, host) {
        Ok(r) => r,
        Err(e) => {
            let _ = net_close(netd_port, socket_id);
            return Err(HttpsError::Tls(e));
        }
    };

    // 4. Install application keys
    stream.set_write_key(hs.client_app.key, hs.client_app.iv);
    stream.set_read_key(hs.server_app.key, hs.server_app.iv);

    // 5. HTTP/1.1 GET (keep-alive off for simplicity)
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: AtomBrowser/0.1\r\nAccept: text/html,text/plain,*/*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    );
    stream.send_encrypted(CT_APPLICATION_DATA, request.as_bytes())?;

    // 6. Accumulate response
    let mut response: Vec<u8> = Vec::new();
    loop {
        match stream.recv_record() {
            Ok(rec) if rec.content_type == CT_APPLICATION_DATA => {
                if rec.data.is_empty() {
                    break;
                }
                if response.len().saturating_add(rec.data.len()) > MAX_RESPONSE_BYTES {
                    let _ = net_close(netd_port, socket_id);
                    return Err(HttpsError::ResponseTooLarge);
                }
                response.extend_from_slice(&rec.data);
            }
            Ok(_) => {}
            Err(RecordError::UnexpectedEof) | Err(RecordError::Io(_)) => break,
            Err(e) => {
                let _ = net_close(netd_port, socket_id);
                return Err(HttpsError::Record(e));
            }
        }
    }
    let _ = net_close(netd_port, socket_id);

    if response.is_empty() {
        return Err(HttpsError::EmptyResponse);
    }

    let status = parse_status(&response);
    let location = parse_location(&response);
    let raw_body = split_body(&response);
    let body = if has_header_token(&response, b"transfer-encoding:", b"chunked") {
        decode_chunked_body(&raw_body)
    } else {
        raw_body
    };

    Ok(HttpsResponse {
        status,
        body,
        location,
        cert_unverified: hs.cert_unverified,
    })
}

// ── HTTP response helpers (minimal copies of libnet/http.rs helpers) ──────────

fn parse_status(r: &[u8]) -> u16 {
    let end = r.windows(2).position(|w| w == b"\r\n").unwrap_or(r.len());
    let line = &r[..end];
    let after = line
        .iter()
        .position(|&b| b == b' ')
        .map(|p| p + 1)
        .unwrap_or(0);
    if after + 3 > line.len() {
        return 0;
    }
    let mut s = 0u16;
    for &b in &line[after..after + 3] {
        if b.is_ascii_digit() {
            s = s * 10 + (b - b'0') as u16;
        } else {
            return 0;
        }
    }
    s
}

fn split_body(r: &[u8]) -> Vec<u8> {
    r.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| r[p + 4..].to_vec())
        .unwrap_or_default()
}

fn parse_location(r: &[u8]) -> Option<String> {
    let end = r.windows(4).position(|w| w == b"\r\n\r\n")?;
    let headers = &r[..end];
    let mut pos = 0;
    while pos < headers.len() {
        let le = headers[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .map(|p| pos + p)
            .unwrap_or(headers.len());
        let line = &headers[pos..le];
        if starts_with_ignore_case(line, b"location:") {
            let mut v = &line[b"location:".len()..];
            while !v.is_empty() && (v[0] == b' ' || v[0] == b'\t') {
                v = &v[1..];
            }
            if let Ok(s) = core::str::from_utf8(v) {
                return Some(String::from(s));
            }
        }
        if le == headers.len() {
            break;
        }
        pos = le + 2;
    }
    None
}

fn starts_with_ignore_case(s: &[u8], prefix: &[u8]) -> bool {
    s.len() >= prefix.len()
        && s[..prefix.len()]
            .iter()
            .zip(prefix)
            .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
}

fn has_header_token(response: &[u8], name: &[u8], token: &[u8]) -> bool {
    let Some(end) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let headers = &response[..end];
    let mut pos = 0;
    while pos < headers.len() {
        let le = headers[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .map(|p| pos + p)
            .unwrap_or(headers.len());
        let line = &headers[pos..le];
        if starts_with_ignore_case(line, name) {
            let mut v = &line[name.len()..];
            while !v.is_empty() && (v[0] == b' ' || v[0] == b'\t') {
                v = &v[1..];
            }
            return !token.is_empty()
                && v.windows(token.len()).any(|w| {
                    w.iter()
                        .zip(token)
                        .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
                });
        }
        if le == headers.len() {
            break;
        }
        pos = le + 2;
    }
    false
}

/// Decode an HTTP/1.1 chunked body.  Falls back to the raw bytes when the
/// framing is malformed and nothing was decoded yet.
fn decode_chunked_body(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let Some(rel) = raw[pos..].windows(2).position(|w| w == b"\r\n") else {
            return if out.is_empty() { raw.to_vec() } else { out };
        };
        let line_end = pos + rel;
        let Some(size) = parse_chunk_size(&raw[pos..line_end]) else {
            return if out.is_empty() { raw.to_vec() } else { out };
        };
        pos = line_end + 2;
        if size == 0 {
            break;
        }
        if pos + size > raw.len() {
            return if out.is_empty() { raw.to_vec() } else { out };
        }
        out.extend_from_slice(&raw[pos..pos + size]);
        pos += size;
        if pos + 2 <= raw.len() && &raw[pos..pos + 2] == b"\r\n" {
            pos += 2;
        } else if pos < raw.len() {
            return out;
        }
    }
    out
}

fn parse_chunk_size(line: &[u8]) -> Option<usize> {
    let mut size = 0usize;
    let mut saw_digit = false;
    for &b in line {
        if b == b';' {
            break;
        }
        let digit = match b {
            b'0'..=b'9' => (b - b'0') as usize,
            b'a'..=b'f' => (b - b'a' + 10) as usize,
            b'A'..=b'F' => (b - b'A' + 10) as usize,
            b' ' | b'\t' if !saw_digit => continue,
            b' ' | b'\t' => break,
            _ => return None,
        };
        saw_digit = true;
        size = size.checked_mul(16)?.checked_add(digit)?;
    }
    if saw_digit {
        Some(size)
    } else {
        None
    }
}
