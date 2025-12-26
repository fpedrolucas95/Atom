//! Serialization Utilities
//!
//! Helper functions for serializing and deserializing primitive types.

/// Read a u32 from a byte slice in little-endian format
pub fn read_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read a u64 from a byte slice in little-endian format
pub fn read_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 8 {
        return None;
    }
    Some(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

/// Read an i32 from a byte slice in little-endian format
pub fn read_i32(bytes: &[u8]) -> Option<i32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read an i16 from a byte slice in little-endian format
pub fn read_i16(bytes: &[u8]) -> Option<i16> {
    if bytes.len() < 2 {
        return None;
    }
    Some(i16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Write a u32 to a byte slice in little-endian format
pub fn write_u32(bytes: &mut [u8], value: u32) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    bytes[0..4].copy_from_slice(&value.to_le_bytes());
    true
}

/// Write a u64 to a byte slice in little-endian format
pub fn write_u64(bytes: &mut [u8], value: u64) -> bool {
    if bytes.len() < 8 {
        return false;
    }
    bytes[0..8].copy_from_slice(&value.to_le_bytes());
    true
}

/// Write an i32 to a byte slice in little-endian format
pub fn write_i32(bytes: &mut [u8], value: i32) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    bytes[0..4].copy_from_slice(&value.to_le_bytes());
    true
}

/// Write an i16 to a byte slice in little-endian format
pub fn write_i16(bytes: &mut [u8], value: i16) -> bool {
    if bytes.len() < 2 {
        return false;
    }
    bytes[0..2].copy_from_slice(&value.to_le_bytes());
    true
}
