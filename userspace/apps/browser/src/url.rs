//! URL normalisation and resolution for plain-HTTP browsing.
//!
//! Atom has no TLS stack, so HTTPS is rewritten to HTTP where possible and
//! otherwise rejected with a clear message. Relative references are resolved
//! against the current page following RFC 3986 path semantics.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::text::starts_with_ignore_ascii_case;

/// A parsed `http://` URL split into its addressable parts.
pub struct HttpTarget {
    pub host: String,
    pub path: String,
    pub port: u16,
}

/// Well-known shorthands that expand to a full plain-HTTP URL.
const SHORTCUTS: &[(&str, &str)] = &[
    ("google", "http://www.google.com/"),
    ("google.com", "http://www.google.com/"),
    ("www.google.com", "http://www.google.com/"),
    ("neverssl", "http://neverssl.com/"),
    ("neverssl.com", "http://neverssl.com/"),
    ("example", "http://example.com/"),
    ("example.com", "http://example.com/"),
];

/// Turn user input into a canonical `http://` URL, or `None` if it isn't a
/// browsable address.
pub fn normalize_http_url(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") {
        return Some(String::from(trimmed));
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        return Some(format!("http://{}", rest));
    }
    if let Some((_, url)) = SHORTCUTS.iter().find(|(name, _)| *name == trimmed) {
        return Some(String::from(*url));
    }
    if trimmed.bytes().any(|b| b == b'.') {
        return Some(format!("http://{}", trimmed));
    }
    None
}

/// Split an `http://` URL into host, path, and port.
pub fn split_http_url(url: &str) -> Result<HttpTarget, String> {
    let Some(rest_with_fragment) = url.strip_prefix("http://") else {
        return Err(String::from("Only plain HTTP is supported"));
    };
    let rest = rest_with_fragment
        .split_once('#')
        .map(|(before, _)| before)
        .unwrap_or(rest_with_fragment);

    let path_start = rest
        .bytes()
        .position(|b| b == b'/' || b == b'?')
        .unwrap_or(rest.len());
    let host_port = &rest[..path_start];
    let path = match rest.as_bytes().get(path_start) {
        None => String::from("/"),
        Some(b'?') => format!("/{}", &rest[path_start..]),
        _ => String::from(&rest[path_start..]),
    };

    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h, parse_port(p).unwrap_or(80)),
        None => (host_port, 80),
    };
    if host.is_empty() {
        return Err(String::from("Invalid HTTP host"));
    }
    Ok(HttpTarget {
        host: String::from(host),
        path,
        port,
    })
}

/// Resolve a redirect `Location` against the current URL.
pub fn normalize_redirect_url(base_url: &str, location: &str) -> Option<String> {
    normalize_http_url(location).or_else(|| resolve_url(base_url, location))
}

/// Resolve a (possibly relative) reference against the page URL. Returns `None`
/// for unsupported schemes (`https:`, `data:` are handled elsewhere).
pub fn resolve_url(base_url: &str, rel: &str) -> Option<String> {
    let rel = rel.trim();
    if rel.is_empty() || rel.starts_with('#') {
        return None;
    }
    if starts_with_ignore_ascii_case(rel.as_bytes(), b"data:") {
        return None;
    }
    if rel.starts_with("http://") {
        return Some(String::from(rel));
    }
    if rel.starts_with("https://") {
        return None;
    }
    if let Some(stripped) = rel.strip_prefix("//") {
        return Some(format!("http://{}", stripped));
    }
    // A scheme before any slash means an unsupported absolute URL.
    if let Some(colon) = rel.find(':') {
        if colon < rel.find('/').unwrap_or(rel.len()) {
            return None;
        }
    }

    let target = split_http_url(base_url).ok()?;
    let hostport = if target.port == 80 {
        target.host
    } else {
        format!("{}:{}", target.host, target.port)
    };
    let base_path = target
        .path
        .split_once('?')
        .map(|(before, _)| before)
        .unwrap_or(&target.path);

    if rel.starts_with('?') {
        return Some(format!("http://{}{}{}", hostport, base_path, rel));
    }

    let (path_part, suffix) = split_path_suffix(rel);
    let resolved = if path_part.starts_with('/') {
        normalize_url_path(path_part)
    } else {
        let dir_end = base_path.rfind('/').map(|i| i + 1).unwrap_or(0);
        normalize_url_path(&format!("{}{}", &base_path[..dir_end], path_part))
    };
    Some(format!("http://{}{}{}", hostport, resolved, suffix))
}

fn split_path_suffix(path: &str) -> (&str, &str) {
    let end = path
        .bytes()
        .position(|b| b == b'?' || b == b'#')
        .unwrap_or(path.len());
    (&path[..end], &path[end..])
}

/// Collapse `.` and `..` segments to canonicalise an absolute path.
fn normalize_url_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    if path.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    out
}

fn parse_port(s: &str) -> Option<u16> {
    let mut value: u32 = 0;
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (b - b'0') as u32;
        if value > u16::MAX as u32 {
            return None;
        }
    }
    Some(value as u16)
}
