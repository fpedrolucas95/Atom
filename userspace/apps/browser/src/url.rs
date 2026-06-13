//! URL normalisation and resolution.
//!
//! Handles both `http://` and `https://` URLs.  Relative references are
//! resolved against the current page following RFC 3986 path semantics.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::text::starts_with_ignore_ascii_case;

/// A parsed HTTP or HTTPS URL split into its addressable parts.
pub struct HttpTarget {
    pub host:  String,
    pub path:  String,
    pub port:  u16,
    pub https: bool,
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

/// Turn user input into a browsable URL (http:// or https://), or `None`.
pub fn normalize_http_url(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(String::from(trimmed));
    }
    if let Some((_, url)) = SHORTCUTS.iter().find(|(name, _)| *name == trimmed) {
        return Some(String::from(*url));
    }
    if trimmed.bytes().any(|b| b == b'.') {
        return Some(format!("http://{}", trimmed));
    }
    None
}

/// Split an `http://` or `https://` URL into host, path, port, and scheme.
pub fn split_http_url(url: &str) -> Result<HttpTarget, String> {
    let (rest_with_fragment, https) = if let Some(r) = url.strip_prefix("https://") {
        (r, true)
    } else if let Some(r) = url.strip_prefix("http://") {
        (r, false)
    } else {
        return Err(String::from("Only HTTP/HTTPS URLs are supported"));
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

    let default_port = if https { 443 } else { 80 };
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h, parse_port(p).unwrap_or(default_port)),
        None => (host_port, default_port),
    };
    if host.is_empty() {
        return Err(String::from("Invalid host"));
    }
    Ok(HttpTarget {
        host: String::from(host),
        path,
        port,
        https,
    })
}

/// Resolve a redirect `Location` against the current URL.
pub fn normalize_redirect_url(base_url: &str, location: &str) -> Option<String> {
    normalize_http_url(location).or_else(|| resolve_url(base_url, location))
}

/// Resolve a (possibly relative) reference against the page URL. Returns `None`
/// for unsupported schemes (`data:` is handled elsewhere).
pub fn resolve_url(base_url: &str, rel: &str) -> Option<String> {
    let rel = rel.trim();
    if rel.is_empty() || rel.starts_with('#') {
        return None;
    }
    if starts_with_ignore_ascii_case(rel.as_bytes(), b"data:") {
        return None;
    }
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return Some(String::from(rel));
    }
    // Protocol-relative reference: inherit scheme from base
    if let Some(stripped) = rel.strip_prefix("//") {
        let scheme = if base_url.starts_with("https://") { "https" } else { "http" };
        return Some(format!("{}://{}", scheme, stripped));
    }
    // A scheme before any slash means an unsupported absolute URL.
    if let Some(colon) = rel.find(':') {
        if colon < rel.find('/').unwrap_or(rel.len()) {
            return None;
        }
    }

    let target = split_http_url(base_url).ok()?;
    let scheme = if target.https { "https" } else { "http" };
    let default_port = if target.https { 443u16 } else { 80u16 };
    let hostport = if target.port == default_port {
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
        return Some(format!("{}://{}{}{}", scheme, hostport, base_path, rel));
    }

    let (path_part, suffix) = split_path_suffix(rel);
    let resolved = if path_part.starts_with('/') {
        normalize_url_path(path_part)
    } else {
        let dir_end = base_path.rfind('/').map(|i| i + 1).unwrap_or(0);
        normalize_url_path(&format!("{}{}", &base_path[..dir_end], path_part))
    };
    Some(format!("{}://{}{}{}", scheme, hostport, resolved, suffix))
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
