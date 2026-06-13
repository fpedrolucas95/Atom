//! Network and resource fetching: HTTP/HTTPS GET (with redirect following),
//! image retrieval, `data:` URI decoding, and image format detection.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libimage::{GifDecoder, ImageDecoder, JpgDecoder, PngDecoder};
use libipc::protocol::lookup_service;

use crate::text::{
    base64_decode, find_ignore_ascii_case, percent_decode, starts_with_ignore_ascii_case,
};
use crate::url::{normalize_redirect_url, split_http_url};

/// Maximum redirects followed before giving up, per resource class.
const MAX_PAGE_REDIRECTS: u32 = 4;
const MAX_IMAGE_REDIRECTS: u32 = 3;

pub struct PageFetch {
    pub body: String,
    pub final_url: String,
    pub cert_unverified: bool,
}

fn is_redirect(status: u16) -> bool {
    (300..400).contains(&status)
}

fn is_success(status: u16) -> bool {
    status == 0 || (200..300).contains(&status)
}

/// Fetch a page body as text, following HTTP and HTTPS redirects.
pub fn fetch_http(url: &str) -> Result<PageFetch, String> {
    let netd = lookup_service("netd").map_err(|_| String::from("netd service not found"))?;

    let mut current = String::from(url);
    let mut cert_warning = false;
    for _ in 0..MAX_PAGE_REDIRECTS {
        let target = split_http_url(&current)?;
        let (status, body, location) = if target.https {
            let resp = libtls::https_get(netd, &target.host, &target.path, target.port)
                .map_err(|e| format!("HTTPS request failed ({:?})", e))?;
            if resp.cert_unverified {
                cert_warning = true;
            }
            (resp.status, resp.body, resp.location)
        } else {
            let resp = libnet::http_get(netd, &target.host, &target.path, target.port)
                .map_err(|e| format!("HTTP request failed ({:?})", e))?;
            (resp.status, resp.body, resp.location)
        };

        if is_redirect(status) {
            if let Some(loc) = location {
                if let Some(next) = normalize_redirect_url(&current, &loc) {
                    current = next;
                    continue;
                }
            }
        }

        if status == 0 && body.is_empty() {
            return Err(String::from("No response received"));
        }
        if body.is_empty() {
            return Err(format!("HTTP {} with empty body", status));
        }
        let page_body = String::from_utf8_lossy(&body).into_owned();
        return Ok(PageFetch {
            body: page_body,
            final_url: current,
            cert_unverified: cert_warning,
        });
    }
    Err(String::from("Too many redirects"))
}

/// Fetch raw bytes (e.g. images), following HTTP and HTTPS redirects.
pub fn fetch_url_bytes(url: &str) -> Option<Vec<u8>> {
    let netd = lookup_service("netd").ok()?;
    let mut current = String::from(url);
    for _ in 0..MAX_IMAGE_REDIRECTS {
        let target = split_http_url(&current).ok()?;
        let (status, body, location) = if target.https {
            let resp = libtls::https_get(netd, &target.host, &target.path, target.port).ok()?;
            (resp.status, resp.body, resp.location)
        } else {
            let resp = libnet::http_get(netd, &target.host, &target.path, target.port).ok()?;
            (resp.status, resp.body, resp.location)
        };
        if is_redirect(status) {
            current = normalize_redirect_url(&current, &location?)?;
            continue;
        }
        if body.is_empty() || !is_success(status) {
            return None;
        }
        return Some(body);
    }
    None
}

/// Decode PNG/JPEG/GIF bytes, using the magic number to pick a decoder first.
pub fn decode_image(bytes: &[u8]) -> Option<libimage::DecodedImage> {
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.starts_with(PNG_MAGIC) {
        return PngDecoder::decode(bytes).ok();
    }
    if bytes.starts_with(b"GIF8") {
        return GifDecoder::decode(bytes).ok();
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        return JpgDecoder::decode(bytes).ok();
    }
    PngDecoder::decode(bytes)
        .or_else(|_| JpgDecoder::decode(bytes))
        .or_else(|_| GifDecoder::decode(bytes))
        .ok()
}

/// Decode a `data:` URI into its raw bytes (base64 or percent-encoded).
pub fn decode_data_uri(uri: &str) -> Option<Vec<u8>> {
    let trimmed = uri.trim_start();
    if !starts_with_ignore_ascii_case(trimmed.as_bytes(), b"data:") {
        return None;
    }
    let rest = &trimmed["data:".len()..];
    let (meta, data) = rest.split_once(',')?;
    if find_ignore_ascii_case(meta.as_bytes(), b"base64").is_some() {
        base64_decode(data)
    } else {
        Some(percent_decode(data))
    }
}
