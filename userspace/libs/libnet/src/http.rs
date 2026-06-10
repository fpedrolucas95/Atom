use crate::{net_close, net_connect, net_recv, net_resolve, net_send, net_socket, NetError};
use alloc::string::String;
use alloc::vec::Vec;
use atom_syscall::ipc::PortId;

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub location: Option<String>,
}

fn copy_bytes(dst: &mut [u8], pos: &mut usize, src: &[u8]) {
    let available = dst.len().saturating_sub(*pos);
    let len = src.len().min(available);
    dst[*pos..*pos + len].copy_from_slice(&src[..len]);
    *pos += len;
}

/// Perform an HTTP/1.0 GET request.
pub fn http_get(
    netd_port: PortId,
    host: &str,
    path: &str,
    port: u16,
) -> Result<HttpResponse, NetError> {
    // 1. Resolve hostname
    let ip = net_resolve(netd_port, host)?;

    // 2. Create socket
    let socket_id = net_socket(netd_port)?;

    // 3. Connect
    net_connect(netd_port, socket_id, ip, port)?;

    // 4. Build HTTP/1.0 request using a fixed stack buffer
    let mut req_buf = [0u8; 512];
    let mut pos = 0usize;
    copy_bytes(&mut req_buf, &mut pos, b"GET ");
    copy_bytes(&mut req_buf, &mut pos, path.as_bytes());
    copy_bytes(&mut req_buf, &mut pos, b" HTTP/1.0\r\nHost: ");
    copy_bytes(&mut req_buf, &mut pos, host.as_bytes());
    copy_bytes(
        &mut req_buf,
        &mut pos,
        b"\r\nUser-Agent: AtomBrowser/0.1\r\nAccept: text/html,text/plain,*/*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
    );

    // 5. Send request
    net_send(netd_port, socket_id, &req_buf[..pos])?;

    // 6. Receive response, accumulating into a Vec
    let mut response: Vec<u8> = Vec::new();
    let mut recv_buf = [0u8; 1024];

    loop {
        match net_recv(netd_port, socket_id, &mut recv_buf, 10000) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&recv_buf[..n]),
            Err(NetError::NotConnected) | Err(NetError::Timeout) => break,
            Err(e) => {
                let _ = net_close(netd_port, socket_id);
                return Err(e);
            }
        }
    }

    // 7. Close socket
    let _ = net_close(netd_port, socket_id);

    // 8. Parse status code from first line: "HTTP/1.x NNN ..."
    let status = parse_status(&response);

    // 9. Extract a redirect target if present.
    let location = parse_location(&response);

    // 10. Find header/body separator "\r\n\r\n" and normalize common HTTP encodings.
    let raw_body = split_body(&response);
    let body = if has_header_token(&response, b"transfer-encoding:", b"chunked") {
        decode_chunked_body(&raw_body)
    } else {
        raw_body
    };

    Ok(HttpResponse {
        status,
        body,
        location,
    })
}

fn parse_status(response: &[u8]) -> u16 {
    // Find first \r\n
    let line_end = response
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(response.len());
    let first_line = &response[..line_end];

    // "HTTP/1.x NNN ..."
    // Find the first space to skip "HTTP/1.x"
    let after_version = first_line
        .iter()
        .position(|&b| b == b' ')
        .map(|p| p + 1)
        .unwrap_or(0);
    if after_version + 3 > first_line.len() {
        return 0;
    }

    let status_bytes = &first_line[after_version..after_version + 3];
    let mut status: u16 = 0;
    for &b in status_bytes {
        if b.is_ascii_digit() {
            status = status * 10 + (b - b'0') as u16;
        } else {
            return 0;
        }
    }
    status
}

fn split_body(response: &[u8]) -> Vec<u8> {
    // Find \r\n\r\n separator
    let separator = b"\r\n\r\n";
    if let Some(pos) = response.windows(4).position(|w| w == separator) {
        response[pos + 4..].to_vec()
    } else {
        Vec::new()
    }
}

fn header_end(response: &[u8]) -> Option<usize> {
    response.windows(4).position(|w| w == b"\r\n\r\n")
}

fn has_header_token(response: &[u8], name: &[u8], token: &[u8]) -> bool {
    let Some(header_end) = header_end(response) else {
        return false;
    };
    let headers = &response[..header_end];
    let mut pos = 0usize;

    while pos < headers.len() {
        let line_end = headers[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .map(|p| pos + p)
            .unwrap_or(headers.len());
        let line = &headers[pos..line_end];

        if starts_with_ignore_ascii_case(line, name) {
            let mut value = &line[name.len()..];
            while !value.is_empty() && (value[0] == b' ' || value[0] == b'\t') {
                value = &value[1..];
            }
            return contains_ignore_ascii_case(value, token);
        }

        if line_end == headers.len() {
            break;
        }
        pos = line_end + 2;
    }

    false
}

fn decode_chunked_body(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0usize;

    loop {
        let Some(line_rel_end) = raw[pos..].windows(2).position(|w| w == b"\r\n") else {
            return if out.is_empty() { raw.to_vec() } else { out };
        };
        let line_end = pos + line_rel_end;
        let size_line = &raw[pos..line_end];
        let Some(size) = parse_chunk_size(size_line) else {
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

fn parse_location(response: &[u8]) -> Option<String> {
    let header_end = response.windows(4).position(|w| w == b"\r\n\r\n")?;
    let headers = &response[..header_end];
    let mut pos = 0usize;

    while pos < headers.len() {
        let line_end = headers[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .map(|p| pos + p)
            .unwrap_or(headers.len());
        let line = &headers[pos..line_end];

        if starts_with_ignore_ascii_case(line, b"location:") {
            let mut value = &line[b"location:".len()..];
            while !value.is_empty() && (value[0] == b' ' || value[0] == b'\t') {
                value = &value[1..];
            }
            if let Ok(s) = core::str::from_utf8(value) {
                return Some(String::from(s));
            }
        }

        if line_end == headers.len() {
            break;
        }
        pos = line_end + 2;
    }

    None
}

fn starts_with_ignore_ascii_case(value: &[u8], prefix: &[u8]) -> bool {
    value.len() >= prefix.len()
        && value[..prefix.len()]
            .iter()
            .zip(prefix.iter())
            .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
}

fn contains_ignore_ascii_case(value: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && value
            .windows(needle.len())
            .any(|w| eq_ignore_ascii_case(w, needle))
}

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
}
