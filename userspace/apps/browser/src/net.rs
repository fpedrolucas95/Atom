//! Network and resource fetching: HTTP GET (with redirect following), image
//! retrieval, `data:` URI decoding, and image format detection.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libimage::{ImageDecoder, JpgDecoder, PngDecoder};
use libipc::protocol::lookup_service;

use crate::text::{
    base64_decode, find_ignore_ascii_case, percent_decode, starts_with_ignore_ascii_case,
};
use crate::url::{normalize_redirect_url, split_http_url};

/// Maximum redirects followed before giving up, per resource class.
const MAX_PAGE_REDIRECTS: u32 = 4;
const MAX_IMAGE_REDIRECTS: u32 = 3;

fn is_redirect(status: u16) -> bool {
    (300..400).contains(&status)
}

fn is_success(status: u16) -> bool {
    status == 0 || (200..300).contains(&status)
}

/// Fetch a page body as text, following plain-HTTP redirects.
pub fn fetch_http(url: &str) -> Result<String, String> {
    let netd = lookup_service("netd").map_err(|_| String::from("netd service not found"))?;

    let mut current = String::from(url);
    for _ in 0..MAX_PAGE_REDIRECTS {
        let target = split_http_url(&current)?;
        let response = libnet::http_get(netd, &target.host, &target.path, target.port)
            .map_err(|e| format!("HTTP request failed ({:?})", e))?;

        if is_redirect(response.status) {
            if let Some(location) = response.location {
                if location.trim().starts_with("https://") {
                    return Err(format!(
                        "This site requires HTTPS, but TLS is not implemented yet: {}",
                        location
                    ));
                }
                if let Some(next) = normalize_redirect_url(&current, &location) {
                    current = next;
                    continue;
                }
            }
        }

        if response.status == 0 && response.body.is_empty() {
            return Err(String::from("No HTTP response received"));
        }
        if response.body.is_empty() {
            return Err(format!("HTTP {} with empty body", response.status));
        }
        return Ok(String::from_utf8_lossy(&response.body).into_owned());
    }
    Err(String::from("Too many HTTP redirects"))
}

/// Fetch raw bytes (e.g. images), following plain-HTTP redirects.
pub fn fetch_url_bytes(url: &str) -> Option<Vec<u8>> {
    let netd = lookup_service("netd").ok()?;
    let mut current = String::from(url);
    for _ in 0..MAX_IMAGE_REDIRECTS {
        let target = split_http_url(&current).ok()?;
        let resp = libnet::http_get(netd, &target.host, &target.path, target.port).ok()?;
        if is_redirect(resp.status) {
            let loc = resp.location?;
            if loc.trim().starts_with("https://") {
                return None;
            }
            current = normalize_redirect_url(&current, &loc)?;
            continue;
        }
        if resp.body.is_empty() || !is_success(resp.status) {
            return None;
        }
        return Some(resp.body);
    }
    None
}

/// Decode PNG/JPEG bytes, using the magic number to pick a decoder first.
pub fn decode_image(bytes: &[u8]) -> Option<libimage::DecodedImage> {
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.starts_with(PNG_MAGIC) {
        return PngDecoder::decode(bytes).ok();
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        return JpgDecoder::decode(bytes).ok();
    }
    PngDecoder::decode(bytes)
        .or_else(|_| JpgDecoder::decode(bytes))
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
