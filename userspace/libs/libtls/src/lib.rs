//! TLS 1.3 client library for Atom userspace.
//! Phase 1: full handshake with no certificate validation.
//! Cipher suite: TLS_CHACHA20_POLY1305_SHA256.  Key exchange: X25519.

#![no_std]
extern crate alloc;

pub mod aead;
pub mod chacha20;
pub mod handshake;
pub mod hkdf;
pub mod hmac;
pub mod poly1305;
pub mod random;
pub mod record;
pub mod sha256;
pub mod x25519;

mod client;

pub use aead::{chacha20_poly1305_open, chacha20_poly1305_seal, AeadError};
pub use chacha20::{chacha20_xor, ChaCha20};
pub use client::{https_get, https_get_with, HttpsError, HttpsResponse};
pub use hkdf::{derive_secret, hkdf_expand, hkdf_expand_label, hkdf_extract};
pub use hmac::{hmac_sha256, hmac_sha256_two};
pub use poly1305::poly1305_mac;
pub use sha256::{sha256, sha256_two, Sha256};
