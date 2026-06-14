//! A small cookie jar shared between the network layer (sending the `Cookie`
//! request header, storing `Set-Cookie`) and the JS runtime (`document.cookie`).
//!
//! Scope model: host-only by default, or a `Domain` cookie matching the host
//! suffix; `Path` prefix match; `Secure` cookies only ride HTTPS. Expiry is
//! honoured via `Max-Age` / `Expires` in the past (deletion); other cookies are
//! treated as session cookies. `HttpOnly` cookies are hidden from
//! `document.cookie` but still sent on requests.
//!
//! Lives in the `js` module so the host-side engine tests can reach it without
//! pulling in the browser's network code; the jar itself does no I/O.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

/// A reference-counted jar shared by the browser and the page runtime.
pub type SharedJar = Rc<RefCell<CookieJar>>;

pub fn shared() -> SharedJar {
    Rc::new(RefCell::new(CookieJar::new()))
}

struct Cookie {
    domain: String,
    path: String,
    name: String,
    value: String,
    secure: bool,
    http_only: bool,
}

#[derive(Default)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a `Set-Cookie` response header for a request to `host`/`path`.
    pub fn set_from_response(&mut self, host: &str, path: &str, header: &str) {
        self.apply(host, path, header, false);
    }

    /// Apply a `document.cookie = "..."` assignment (cannot set `HttpOnly`).
    pub fn set_from_js(&mut self, host: &str, path: &str, header: &str) {
        self.apply(host, path, header, true);
    }

    fn apply(&mut self, host: &str, path: &str, header: &str, from_js: bool) {
        let mut parts = header.split(';');
        let Some(first) = parts.next() else {
            return;
        };
        let (name, value) = match first.split_once('=') {
            Some((n, v)) => (n.trim().to_string(), v.trim().to_string()),
            None => return,
        };
        if name.is_empty() {
            return;
        }

        let mut domain = host.to_string();
        let mut cpath = default_path(path);
        let mut secure = false;
        let mut http_only = false;
        let mut expired = false;
        for attr in parts {
            let attr = attr.trim();
            let (key, val) = match attr.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (attr, ""),
            };
            match key.to_ascii_lowercase().as_str() {
                "domain" if !val.is_empty() => domain = val.trim_start_matches('.').to_string(),
                "path" if val.starts_with('/') => cpath = val.to_string(),
                "secure" => secure = true,
                // A `document.cookie` write cannot set HttpOnly.
                "httponly" => http_only = !from_js,
                "max-age" => {
                    if val.trim_start_matches('-').parse::<i64>().unwrap_or(1) <= 0 {
                        expired = true;
                    }
                }
                // We don't parse dates; the common deletion form uses a year
                // in the past (1970/1969).
                "expires" => expired |= val.contains("1970") || val.contains("1969"),
                _ => {}
            }
        }

        // Update an existing cookie with the same identity in place (preserving
        // order), delete it on expiry, or append a new one.
        let pos = self
            .cookies
            .iter()
            .position(|c| c.name == name && c.domain == domain && c.path == cpath);
        match (pos, expired) {
            (Some(i), true) => {
                self.cookies.remove(i);
            }
            (Some(i), false) => {
                let c = &mut self.cookies[i];
                c.value = value;
                c.secure = secure;
                c.http_only = http_only;
            }
            (None, false) => self.cookies.push(Cookie {
                domain,
                path: cpath,
                name,
                value,
                secure,
                http_only,
            }),
            (None, true) => {}
        }
    }

    /// The `Cookie:` header value for a request, or `None` if no cookie matches.
    pub fn request_header(&self, host: &str, path: &str, https: bool) -> Option<String> {
        let s = self.join_matching(host, path, https, true);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// The `document.cookie` string (excludes `HttpOnly` cookies).
    pub fn document_cookie(&self, host: &str, path: &str, https: bool) -> String {
        self.join_matching(host, path, https, false)
    }

    fn join_matching(
        &self,
        host: &str,
        path: &str,
        https: bool,
        include_http_only: bool,
    ) -> String {
        let mut out = String::new();
        for c in &self.cookies {
            if c.secure && !https {
                continue;
            }
            if c.http_only && !include_http_only {
                continue;
            }
            if !domain_match(host, &c.domain) || !path_match(path, &c.path) {
                continue;
            }
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(&c.name);
            out.push('=');
            out.push_str(&c.value);
        }
        out
    }
}

/// `Path` defaults to the directory of the request path.
fn default_path(path: &str) -> String {
    let p = path.split(['?', '#']).next().unwrap_or("/");
    match p.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => p[..i].to_string(),
    }
}

fn domain_match(host: &str, domain: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    host == domain || host.ends_with(&alloc::format!(".{domain}"))
}

fn path_match(req: &str, cookie_path: &str) -> bool {
    let req = req.split(['?', '#']).next().unwrap_or("/");
    if cookie_path == "/" {
        return true;
    }
    req == cookie_path
        || (req.starts_with(cookie_path)
            && (cookie_path.ends_with('/') || req[cookie_path.len()..].starts_with('/')))
}
