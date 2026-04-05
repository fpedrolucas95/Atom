use alloc::vec::Vec;
use atom_syscall::ipc::PortId;
use crate::{NetError, net_socket, net_connect, net_send, net_recv, net_close, net_resolve};

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

fn copy_bytes(dst: &mut [u8], pos: &mut usize, src: &[u8]) {
    let available = dst.len().saturating_sub(*pos);
    let len = src.len().min(available);
    dst[*pos..*pos + len].copy_from_slice(&src[..len]);
    *pos += len;
}

/// Perform an HTTP/1.0 GET request.
pub fn http_get(netd_port: PortId, host: &str, path: &str, port: u16) -> Result<HttpResponse, NetError> {
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
    copy_bytes(&mut req_buf, &mut pos, b"\r\nConnection: close\r\n\r\n");

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

    // 9. Find header/body separator "\r\n\r\n"
    let body = split_body(&response);

    Ok(HttpResponse { status, body })
}

fn parse_status(response: &[u8]) -> u16 {
    // Find first \r\n
    let line_end = response.windows(2).position(|w| w == b"\r\n").unwrap_or(response.len());
    let first_line = &response[..line_end];

    // "HTTP/1.x NNN ..."
    // Find the first space to skip "HTTP/1.x"
    let after_version = first_line.iter().position(|&b| b == b' ').map(|p| p + 1).unwrap_or(0);
    if after_version + 3 > first_line.len() {
        return 0;
    }

    let status_bytes = &first_line[after_version..after_version + 3];
    let mut status: u16 = 0;
    for &b in status_bytes {
        if b >= b'0' && b <= b'9' {
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
