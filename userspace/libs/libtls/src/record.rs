//! TLS 1.3 record layer: framing, buffering, and AEAD encryption/decryption.

use alloc::vec::Vec;
use atom_syscall::ipc::PortId;
use libnet::{net_recv, net_send, NetError};

use crate::aead::{chacha20_poly1305_open, chacha20_poly1305_seal, AeadError};

// ── Content types ─────────────────────────────────────────────────────────────
pub const CT_CHANGE_CIPHER_SPEC: u8 = 20;
pub const CT_ALERT: u8 = 21;
pub const CT_HANDSHAKE: u8 = 22;
pub const CT_APPLICATION_DATA: u8 = 23;

/// A decrypted TLS record delivered to the caller.
pub struct Record {
    pub content_type: u8,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub enum RecordError {
    Io(NetError),
    Aead(AeadError),
    Overflow,
    UnexpectedEof,
    BadRecord,
}

impl From<NetError> for RecordError {
    fn from(e: NetError) -> Self {
        Self::Io(e)
    }
}
impl From<AeadError> for RecordError {
    fn from(e: AeadError) -> Self {
        Self::Aead(e)
    }
}

/// Owns the TCP socket and manages per-direction sequence numbers + keys.
pub struct RecordStream {
    netd_port: PortId,
    socket_id: u32,
    // Write direction
    write_key: Option<[u8; 32]>,
    write_iv: Option<[u8; 12]>,
    write_seq: u64,
    // Read direction
    read_key: Option<[u8; 32]>,
    read_iv: Option<[u8; 12]>,
    read_seq: u64,
    // Incoming byte buffer
    incoming: Vec<u8>,
}

impl RecordStream {
    pub fn new(netd_port: PortId, socket_id: u32) -> Self {
        Self {
            netd_port,
            socket_id,
            write_key: None,
            write_iv: None,
            write_seq: 0,
            read_key: None,
            read_iv: None,
            read_seq: 0,
            incoming: Vec::new(),
        }
    }

    /// Install the handshake write key (client → server direction).
    pub fn set_write_key(&mut self, key: [u8; 32], iv: [u8; 12]) {
        self.write_key = Some(key);
        self.write_iv = Some(iv);
        self.write_seq = 0;
    }

    /// Install the handshake read key (server → client direction).
    pub fn set_read_key(&mut self, key: [u8; 32], iv: [u8; 12]) {
        self.read_key = Some(key);
        self.read_iv = Some(iv);
        self.read_seq = 0;
    }

    /// Send a plaintext record (no encryption).  Used before any keys are set.
    pub fn send_plain(&mut self, content_type: u8, data: &[u8]) -> Result<(), RecordError> {
        let mut rec = [0u8; 5];
        rec[0] = content_type;
        rec[1] = 0x03;
        rec[2] = 0x03;
        let len = data.len() as u16;
        rec[3] = (len >> 8) as u8;
        rec[4] = len as u8;
        net_send(self.netd_port, self.socket_id, &rec)?;
        net_send(self.netd_port, self.socket_id, data)?;
        Ok(())
    }

    /// Send an encrypted record.  Requires write keys installed.
    pub fn send_encrypted(&mut self, content_type: u8, data: &[u8]) -> Result<(), RecordError> {
        let key = self.write_key.as_ref().expect("write key not set");
        let iv = self.write_iv.as_ref().expect("write iv not set");
        let nonce = xor_nonce(iv, self.write_seq);
        self.write_seq += 1;

        // TLSInnerPlaintext = data || content_type
        let mut inner = Vec::with_capacity(data.len() + 1);
        inner.extend_from_slice(data);
        inner.push(content_type);

        // AAD = TLSCiphertext header (type=23, version=0x0303, length)
        let ct_len = (inner.len() + 16) as u16; // +16 for AEAD tag
        let aad = [
            CT_APPLICATION_DATA,
            0x03,
            0x03,
            (ct_len >> 8) as u8,
            ct_len as u8,
        ];

        let ciphertext = chacha20_poly1305_seal(key, &nonce, &aad, &inner);

        let mut header = [0u8; 5];
        header[0] = CT_APPLICATION_DATA;
        header[1] = 0x03;
        header[2] = 0x03;
        let clen = ciphertext.len() as u16;
        header[3] = (clen >> 8) as u8;
        header[4] = clen as u8;
        net_send(self.netd_port, self.socket_id, &header)?;
        net_send(self.netd_port, self.socket_id, &ciphertext)?;
        Ok(())
    }

    /// Receive the next TLS record, decrypting if keys are set.
    /// Blocks until a complete record is available.
    pub fn recv_record(&mut self) -> Result<Record, RecordError> {
        loop {
            // Try to parse a complete record from the buffer
            if self.incoming.len() >= 5 {
                let ct = self.incoming[0];
                let len = u16::from_be_bytes([self.incoming[3], self.incoming[4]]) as usize;
                if len > 16384 + 256 {
                    return Err(RecordError::Overflow);
                }
                if self.incoming.len() >= 5 + len {
                    let body: Vec<u8> = self.incoming[5..5 + len].to_vec();
                    self.incoming.drain(..5 + len);

                    // Skip ChangeCipherSpec transparently (middlebox compat)
                    if ct == CT_CHANGE_CIPHER_SPEC {
                        continue;
                    }

                    // If read key is installed and record is ApplicationData, decrypt it
                    if ct == CT_APPLICATION_DATA {
                        if let (Some(key), Some(iv)) = (&self.read_key, &self.read_iv) {
                            let nonce = xor_nonce(iv, self.read_seq);
                            self.read_seq += 1;
                            let aad =
                                [CT_APPLICATION_DATA, 0x03, 0x03, (len >> 8) as u8, len as u8];
                            let plain = chacha20_poly1305_open(key, &nonce, &aad, &body)?;
                            // Strip TLSInnerPlaintext padding: last non-zero byte is real type
                            let (real_type, content_len) = strip_padding(&plain)?;
                            let content = plain[..content_len].to_vec();
                            return Ok(Record {
                                content_type: real_type,
                                data: content,
                            });
                        }
                    }
                    return Ok(Record {
                        content_type: ct,
                        data: body,
                    });
                }
            }
            // Need more data
            self.fill_buf()?;
        }
    }

    fn fill_buf(&mut self) -> Result<(), RecordError> {
        let mut tmp = [0u8; 4096];
        let n =
            net_recv(self.netd_port, self.socket_id, &mut tmp, 30_000).map_err(RecordError::Io)?;
        if n == 0 {
            return Err(RecordError::UnexpectedEof);
        }
        self.incoming.extend_from_slice(&tmp[..n]);
        Ok(())
    }
}

/// Strip zero-padding from TLSInnerPlaintext; return type and content length.
fn strip_padding(data: &[u8]) -> Result<(u8, usize), RecordError> {
    for i in (0..data.len()).rev() {
        if data[i] != 0 {
            return Ok((data[i], i));
        }
    }
    Err(RecordError::BadRecord)
}

/// TLS 1.3 nonce construction: IV XOR (sequence as 96-bit big-endian).
fn xor_nonce(iv: &[u8; 12], seq: u64) -> [u8; 12] {
    let mut nonce = *iv;
    let seq_be = seq.to_be_bytes(); // 8 bytes
                                    // XOR into the last 8 bytes of the 12-byte IV
    for i in 0..8 {
        nonce[4 + i] ^= seq_be[i];
    }
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_inner_plaintext_padding_is_not_part_of_content() {
        assert_eq!(
            strip_padding(&[b'a', b'b', CT_HANDSHAKE, 0, 0]).unwrap(),
            (CT_HANDSHAKE, 2)
        );
    }
}
