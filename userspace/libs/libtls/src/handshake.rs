//! TLS 1.3 handshake state machine.
//! Cipher suite: TLS_CHACHA20_POLY1305_SHA256.  Key exchange: X25519.
//! Phase 1: no certificate validation (server certificate is accepted as-is).

use alloc::vec::Vec;

use crate::hkdf::{derive_secret, hkdf_expand_label, hkdf_extract};
use crate::hmac::hmac_sha256;
use crate::random::random_bytes;
use crate::record::{
    RecordError, RecordStream, CT_APPLICATION_DATA, CT_HANDSHAKE,
};
use crate::sha256::{sha256, Sha256};
use crate::x25519::{x25519, x25519_public};

// ── Handshake message types ───────────────────────────────────────────────────
const HT_CLIENT_HELLO:         u8 = 1;
const HT_SERVER_HELLO:         u8 = 2;
const HT_ENCRYPTED_EXTENSIONS: u8 = 8;
const HT_CERTIFICATE:          u8 = 11;
const HT_CERTIFICATE_VERIFY:   u8 = 15;
const HT_FINISHED:             u8 = 20;

// ── Extension types ───────────────────────────────────────────────────────────
const EXT_SERVER_NAME:         u16 = 0;
const EXT_SUPPORTED_GROUPS:    u16 = 10;
const EXT_SIGNATURE_ALGORITHMS:u16 = 13;
const EXT_SUPPORTED_VERSIONS:  u16 = 43;
const EXT_KEY_SHARE:           u16 = 51;

// ── NamedGroup ────────────────────────────────────────────────────────────────
const GROUP_X25519: u16 = 0x001d;

#[derive(Debug)]
pub enum HandshakeError {
    Record(RecordError),
    UnexpectedMessage,
    BadServerHello,
    NoKeyShare,
    BadFinished,
    AlertReceived(u8),
}
impl From<RecordError> for HandshakeError {
    fn from(e: RecordError) -> Self { Self::Record(e) }
}

/// Traffic key material for one direction.
pub struct TrafficKeys {
    pub key: [u8; 32],
    pub iv:  [u8; 12],
}

/// Secrets needed after the handshake completes.
pub struct HandshakeResult {
    pub client_app: TrafficKeys,
    pub server_app: TrafficKeys,
    /// True if the server certificate was NOT validated (always true in Phase 1).
    pub cert_unverified: bool,
}

/// Run the full TLS 1.3 handshake on `stream`.
/// On success, returns the application-layer keys and leaves the stream
/// ready to send/receive application data.
pub fn run_handshake(
    stream: &mut RecordStream,
    hostname: &str,
) -> Result<HandshakeResult, HandshakeError> {
    // 1. Ephemeral keypair
    let mut priv_key = [0u8; 32];
    random_bytes(&mut priv_key);
    let pub_key = x25519_public(&priv_key);

    // 2. Build and send ClientHello
    let mut client_random = [0u8; 32];
    random_bytes(&mut client_random);
    let mut session_id = [0u8; 32];
    random_bytes(&mut session_id);

    let ch_body = build_client_hello(&client_random, &session_id, hostname, &pub_key);
    let ch_hs = wrap_handshake(HT_CLIENT_HELLO, &ch_body);
    stream.send_plain(CT_HANDSHAKE, &ch_hs)?;

    // Start transcript with ClientHello
    let mut transcript = Sha256::new();
    transcript.update(&ch_hs);

    // 3. Receive ServerHello
    let sh_record = stream.recv_record()?;
    if sh_record.content_type != CT_HANDSHAKE {
        return Err(HandshakeError::UnexpectedMessage);
    }
    let server_pub = parse_server_hello(&sh_record.data)?;
    transcript.update(&sh_record.data);

    // 4. Key schedule: derive handshake secrets
    let zeros_32 = [0u8; 32];
    let empty_hash = sha256(&[]);

    let early_secret  = hkdf_extract(&zeros_32, &zeros_32);
    let derived_early = derive_secret(&early_secret, "derived", &empty_hash);
    let ecdhe         = x25519(&priv_key, &server_pub);
    let hs_secret     = hkdf_extract(&derived_early, &ecdhe);

    // Transcript hash after ServerHello
    let transcript_sh = transcript.clone().finalize();

    let c_hs_ts = derive_secret(&hs_secret, "c hs traffic", &transcript_sh);
    let s_hs_ts = derive_secret(&hs_secret, "s hs traffic", &transcript_sh);

    let s_write_key = traffic_key(&s_hs_ts);
    let s_write_iv  = traffic_iv(&s_hs_ts);
    stream.set_read_key(s_write_key, s_write_iv);

    // Master secret (computed now, keys derived after server Finished)
    let derived_hs  = derive_secret(&hs_secret, "derived", &empty_hash);
    let master_secret = hkdf_extract(&derived_hs, &zeros_32);

    // 5. Receive encrypted handshake messages.  Messages may be split across
    //    or coalesced within records, so reassemble them in a byte buffer and
    //    only consume complete messages from the front.
    let mut hs_buf: Vec<u8> = Vec::new();
    let mut got_ee       = false;
    let mut got_finished = false;
    let mut server_finished_verify = [0u8; 32];

    while !got_finished {
        let rec = stream.recv_record()?;
        match rec.content_type {
            CT_HANDSHAKE => {
                hs_buf.extend_from_slice(&rec.data);
                let mut pos = 0;
                while pos + 4 <= hs_buf.len() {
                    let msg_type = hs_buf[pos];
                    let len = u24_to_usize(&hs_buf[pos + 1..pos + 4]);
                    if pos + 4 + len > hs_buf.len() {
                        break; // incomplete — wait for the next record
                    }
                    let msg = &hs_buf[pos..pos + 4 + len];
                    let body = &hs_buf[pos + 4..pos + 4 + len];

                    match msg_type {
                        HT_ENCRYPTED_EXTENSIONS => {
                            transcript.update(msg);
                            got_ee = true;
                        }
                        HT_CERTIFICATE | HT_CERTIFICATE_VERIFY => {
                            // Phase 1: accept without validation
                            transcript.update(msg);
                        }
                        HT_FINISHED => {
                            if !got_ee {
                                return Err(HandshakeError::UnexpectedMessage);
                            }
                            // Verify server Finished
                            let transcript_before_sf = transcript.clone().finalize();
                            let finished_key = hkdf_expand_label(&s_hs_ts, "finished", &[], 32);
                            let expected = hmac_sha256(&finished_key, &transcript_before_sf);
                            if body.len() < 32 || body[..32] != expected {
                                return Err(HandshakeError::BadFinished);
                            }
                            server_finished_verify.copy_from_slice(&body[..32]);
                            transcript.update(msg);
                            got_finished = true;
                        }
                        _ => {
                            // New session ticket etc. — skip
                            transcript.update(msg);
                        }
                    }
                    pos += 4 + len;
                    if got_finished {
                        break;
                    }
                }
                hs_buf.drain(..pos);
            }
            CT_APPLICATION_DATA => {
                // Encrypted record already decrypted by RecordStream.
                // Treat as additional handshake data.
            }
            21 => {
                // Alert
                let alert_level = if rec.data.is_empty() { 0 } else { rec.data[0] };
                let alert_desc  = if rec.data.len() < 2 { 0 } else { rec.data[1] };
                let _ = alert_level;
                return Err(HandshakeError::AlertReceived(alert_desc));
            }
            _ => {}
        }
    }

    // 6. Transcript hash after server Finished = hash for application secrets
    let transcript_sf = transcript.clone().finalize();

    // 7. Derive application secrets
    let c_ap_ts = derive_secret(&master_secret, "c ap traffic", &transcript_sf);
    let s_ap_ts = derive_secret(&master_secret, "s ap traffic", &transcript_sf);

    // 8. Switch write key to client handshake traffic secret, send client Finished
    let c_write_key = traffic_key(&c_hs_ts);
    let c_write_iv  = traffic_iv(&c_hs_ts);
    stream.set_write_key(c_write_key, c_write_iv);

    // Send ChangeCipherSpec (middlebox compat, TLS 1.3 §D.4)
    stream.send_plain(crate::record::CT_CHANGE_CIPHER_SPEC, &[1u8])?;

    // Compute client Finished
    let client_finished_key = hkdf_expand_label(&c_hs_ts, "finished", &[], 32);
    let client_verify_data  = hmac_sha256(&client_finished_key, &transcript_sf);
    let cf_body = wrap_handshake(HT_FINISHED, &client_verify_data);
    stream.send_encrypted(CT_HANDSHAKE, &cf_body)?;

    // 9. Switch to application keys
    let server_app = TrafficKeys {
        key: traffic_key(&s_ap_ts),
        iv:  traffic_iv(&s_ap_ts),
    };
    let client_app = TrafficKeys {
        key: traffic_key(&c_ap_ts),
        iv:  traffic_iv(&c_ap_ts),
    };

    Ok(HandshakeResult {
        client_app,
        server_app,
        cert_unverified: true,
    })
}

// ── Key derivation helpers ────────────────────────────────────────────────────

fn traffic_key(ts: &[u8; 32]) -> [u8; 32] {
    let v = hkdf_expand_label(ts, "key", &[], 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    out
}

fn traffic_iv(ts: &[u8; 32]) -> [u8; 12] {
    let v = hkdf_expand_label(ts, "iv", &[], 12);
    let mut out = [0u8; 12];
    out.copy_from_slice(&v);
    out
}

// ── ClientHello builder ───────────────────────────────────────────────────────

fn build_client_hello(
    random:     &[u8; 32],
    session_id: &[u8; 32],
    hostname:   &str,
    pub_key:    &[u8; 32],
) -> Vec<u8> {
    let mut exts: Vec<u8> = Vec::new();

    // server_name (0)
    if !hostname.is_empty() {
        let name = hostname.as_bytes();
        let entry_len = 1 + 2 + name.len(); // type(1) + len(2) + name
        let list_len  = entry_len;
        let mut sni = Vec::new();
        write_u16(&mut sni, list_len as u16);
        sni.push(0); // name_type: host_name
        write_u16(&mut sni, name.len() as u16);
        sni.extend_from_slice(name);
        push_extension(&mut exts, EXT_SERVER_NAME, &sni);
    }

    // supported_versions (43): TLS 1.3 only
    let sv: Vec<u8> = {
        let mut v = Vec::new();
        v.push(2); // 1 version × 2 bytes
        write_u16(&mut v, 0x0304);
        v
    };
    push_extension(&mut exts, EXT_SUPPORTED_VERSIONS, &sv);

    // supported_groups (10): x25519
    let sg: Vec<u8> = {
        let mut v = Vec::new();
        write_u16(&mut v, 2); // 1 group × 2 bytes
        write_u16(&mut v, GROUP_X25519);
        v
    };
    push_extension(&mut exts, EXT_SUPPORTED_GROUPS, &sg);

    // signature_algorithms (13)
    let sa: Vec<u8> = {
        let mut v = Vec::new();
        write_u16(&mut v, 4); // 2 algorithms × 2 bytes
        write_u16(&mut v, 0x0804); // rsa_pss_rsae_sha256
        write_u16(&mut v, 0x0403); // ecdsa_secp256r1_sha256
        v
    };
    push_extension(&mut exts, EXT_SIGNATURE_ALGORITHMS, &sa);

    // key_share (51): x25519 public key
    let ks: Vec<u8> = {
        let mut v = Vec::new();
        let entry_len = 2 + 2 + 32u16; // group + key_len + key
        write_u16(&mut v, entry_len + 4); // outer list length (group(2)+len(2)+key(32) = 36)
        write_u16(&mut v, GROUP_X25519);
        write_u16(&mut v, 32);
        v.extend_from_slice(pub_key);
        v
    };
    push_extension(&mut exts, EXT_KEY_SHARE, &ks);

    let mut body: Vec<u8> = Vec::new();
    // legacy_version = TLS 1.2 (0x0303)
    write_u16(&mut body, 0x0303);
    // random
    body.extend_from_slice(random);
    // legacy_session_id
    body.push(32);
    body.extend_from_slice(session_id);
    // cipher_suites: TLS_CHACHA20_POLY1305_SHA256 (0x1303)
    write_u16(&mut body, 2); // list length in bytes
    write_u16(&mut body, 0x1303);
    // compression_methods: null (0)
    body.push(1);
    body.push(0);
    // extensions
    write_u16(&mut body, exts.len() as u16);
    body.extend_from_slice(&exts);

    body
}

fn push_extension(buf: &mut Vec<u8>, ext_type: u16, data: &[u8]) {
    write_u16(buf, ext_type);
    write_u16(buf, data.len() as u16);
    buf.extend_from_slice(data);
}

// ── ServerHello parser ────────────────────────────────────────────────────────

/// Parse ServerHello handshake body (or a stream of handshake messages).
/// Returns the server's x25519 public key from the key_share extension.
fn parse_server_hello(data: &[u8]) -> Result<[u8; 32], HandshakeError> {
    let mut pos = 0;
    // Expect exactly one handshake message: ServerHello
    if pos + 4 > data.len() {
        return Err(HandshakeError::BadServerHello);
    }
    let msg_type = data[pos]; pos += 1;
    if msg_type != HT_SERVER_HELLO {
        return Err(HandshakeError::BadServerHello);
    }
    let msg_len = u24_to_usize(&data[pos..pos + 3]); pos += 3;
    if pos + msg_len > data.len() {
        return Err(HandshakeError::BadServerHello);
    }

    // legacy_version (2), random (32)
    if pos + 2 + 32 > data.len() { return Err(HandshakeError::BadServerHello); }
    pos += 2 + 32;

    // legacy_session_id_echo
    if pos >= data.len() { return Err(HandshakeError::BadServerHello); }
    let sid_len = data[pos] as usize; pos += 1 + sid_len;

    // cipher_suite (2)
    pos += 2;
    // compression_method (1)
    pos += 1;

    if pos + 2 > data.len() { return Err(HandshakeError::BadServerHello); }
    let exts_len = u16_at(data, pos) as usize; pos += 2;
    let exts_end = pos + exts_len;

    while pos + 4 <= exts_end.min(data.len()) {
        let ext_type = u16_at(data, pos); pos += 2;
        let ext_len  = u16_at(data, pos) as usize; pos += 2;
        if pos + ext_len > data.len() { break; }
        let ext_data = &data[pos..pos + ext_len];

        if ext_type == EXT_KEY_SHARE {
            // KeyShareEntry: group(2) + key_length(2) + key
            if ext_data.len() >= 4 {
                let group = u16::from_be_bytes([ext_data[0], ext_data[1]]);
                let key_len = u16::from_be_bytes([ext_data[2], ext_data[3]]) as usize;
                if group == GROUP_X25519 && key_len == 32 && ext_data.len() >= 4 + 32 {
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&ext_data[4..36]);
                    return Ok(k);
                }
            }
        }
        pos += ext_len;
    }
    Err(HandshakeError::NoKeyShare)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn wrap_handshake(msg_type: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.push(msg_type);
    let len = body.len();
    out.push((len >> 16) as u8);
    out.push((len >>  8) as u8);
    out.push( len        as u8);
    out.extend_from_slice(body);
    out
}

fn write_u16(buf: &mut Vec<u8>, v: u16) {
    buf.push((v >> 8) as u8);
    buf.push( v       as u8);
}

fn u16_at(data: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([data[pos], data[pos + 1]])
}

fn u24_to_usize(b: &[u8]) -> usize {
    ((b[0] as usize) << 16) | ((b[1] as usize) << 8) | (b[2] as usize)
}
