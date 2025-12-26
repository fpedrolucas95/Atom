// Message Protocol Definitions
//
// This module defines the binary message format for IPC communication.
// All messages have a common header followed by type-specific payload.

use alloc::vec::Vec;

/// Maximum message payload size (must match kernel limit)
pub const MAX_PAYLOAD_SIZE: usize = 256;

/// Message header size in bytes
pub const HEADER_SIZE: usize = 8;

/// Message types for desktop environment communication
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MessageType {
    // System messages (0x0000 - 0x00FF)
    Ping = 0x0001,
    Pong = 0x0002,
    Error = 0x00FF,

    // Input events (0x0100 - 0x01FF)
    KeyPress = 0x0100,
    KeyRelease = 0x0101,
    MouseMove = 0x0110,
    MouseButtonPress = 0x0111,
    MouseButtonRelease = 0x0112,
    MouseScroll = 0x0113,

    // Window management (0x0200 - 0x02FF)
    WindowCreate = 0x0200,
    WindowCreated = 0x0201,
    WindowDestroy = 0x0202,
    WindowDestroyed = 0x0203,
    WindowResize = 0x0204,
    WindowResized = 0x0205,
    WindowFocus = 0x0206,
    WindowUnfocus = 0x0207,

    // Surface operations (0x0300 - 0x03FF)
    SurfaceCreate = 0x0300,
    SurfaceCreated = 0x0301,
    SurfaceDestroy = 0x0302,
    SurfacePresent = 0x0303,
    SurfaceDamage = 0x0304,
    SurfaceBuffer = 0x0305,

    // Application lifecycle (0x0400 - 0x04FF)
    AppRegister = 0x0400,
    AppRegistered = 0x0401,
    AppUnregister = 0x0402,
    AppFocused = 0x0403,
    AppUnfocused = 0x0404,

    // Service discovery (0x0500 - 0x05FF)
    ServiceLookup = 0x0500,
    ServiceFound = 0x0501,
    ServiceNotFound = 0x0502,

    // Unknown/invalid
    Unknown = 0xFFFF,
}

impl MessageType {
    pub fn from_u32(value: u32) -> Self {
        match value {
            0x0001 => MessageType::Ping,
            0x0002 => MessageType::Pong,
            0x00FF => MessageType::Error,

            0x0100 => MessageType::KeyPress,
            0x0101 => MessageType::KeyRelease,
            0x0110 => MessageType::MouseMove,
            0x0111 => MessageType::MouseButtonPress,
            0x0112 => MessageType::MouseButtonRelease,
            0x0113 => MessageType::MouseScroll,

            0x0200 => MessageType::WindowCreate,
            0x0201 => MessageType::WindowCreated,
            0x0202 => MessageType::WindowDestroy,
            0x0203 => MessageType::WindowDestroyed,
            0x0204 => MessageType::WindowResize,
            0x0205 => MessageType::WindowResized,
            0x0206 => MessageType::WindowFocus,
            0x0207 => MessageType::WindowUnfocus,

            0x0300 => MessageType::SurfaceCreate,
            0x0301 => MessageType::SurfaceCreated,
            0x0302 => MessageType::SurfaceDestroy,
            0x0303 => MessageType::SurfacePresent,
            0x0304 => MessageType::SurfaceDamage,
            0x0305 => MessageType::SurfaceBuffer,

            0x0400 => MessageType::AppRegister,
            0x0401 => MessageType::AppRegistered,
            0x0402 => MessageType::AppUnregister,
            0x0403 => MessageType::AppFocused,
            0x0404 => MessageType::AppUnfocused,

            0x0500 => MessageType::ServiceLookup,
            0x0501 => MessageType::ServiceFound,
            0x0502 => MessageType::ServiceNotFound,

            _ => MessageType::Unknown,
        }
    }
}

/// Message header - prepended to all messages
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MessageHeader {
    /// Message type (4 bytes)
    pub msg_type: u32,
    /// Payload length in bytes (4 bytes)
    pub payload_len: u32,
}

impl MessageHeader {
    pub fn new(msg_type: MessageType, payload_len: usize) -> Self {
        Self {
            msg_type: msg_type as u32,
            payload_len: payload_len as u32,
        }
    }

    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut bytes = [0u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.msg_type.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.payload_len.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_SIZE {
            return None;
        }
        Some(Self {
            msg_type: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            payload_len: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }

    pub fn message_type(&self) -> MessageType {
        MessageType::from_u32(self.msg_type)
    }
}

/// Complete message with header and payload
#[derive(Debug, Clone)]
pub struct Message {
    pub header: MessageHeader,
    pub payload: Vec<u8>,
}

impl Message {
    pub fn new(msg_type: MessageType, payload: Vec<u8>) -> Self {
        Self {
            header: MessageHeader::new(msg_type, payload.len()),
            payload,
        }
    }

    pub fn ping() -> Self {
        Self::new(MessageType::Ping, Vec::new())
    }

    pub fn pong() -> Self {
        Self::new(MessageType::Pong, Vec::new())
    }

    pub fn error(code: u32) -> Self {
        Self::new(MessageType::Error, code.to_le_bytes().to_vec())
    }

    /// Serialize message to bytes for sending
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        bytes.extend_from_slice(&self.header.to_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    /// Deserialize message from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let header = MessageHeader::from_bytes(bytes)?;
        let payload_start = HEADER_SIZE;
        let payload_end = payload_start + header.payload_len as usize;

        if bytes.len() < payload_end {
            return None;
        }

        Some(Self {
            header,
            payload: bytes[payload_start..payload_end].to_vec(),
        })
    }

    pub fn message_type(&self) -> MessageType {
        self.header.message_type()
    }
}

// ============================================================================
// Input Event Messages
// ============================================================================

/// Key event payload
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct KeyEventPayload {
    /// Raw scancode
    pub scancode: u8,
    /// ASCII character (0 if not printable)
    pub ascii: u8,
    /// Modifier flags
    pub modifiers: u8,
    /// Reserved
    pub _reserved: u8,
}

impl KeyEventPayload {
    pub const MODIFIER_SHIFT: u8 = 0x01;
    pub const MODIFIER_CTRL: u8 = 0x02;
    pub const MODIFIER_ALT: u8 = 0x04;

    pub fn new(scancode: u8, ascii: u8, shift: bool, ctrl: bool, alt: bool) -> Self {
        let mut modifiers = 0;
        if shift { modifiers |= Self::MODIFIER_SHIFT; }
        if ctrl { modifiers |= Self::MODIFIER_CTRL; }
        if alt { modifiers |= Self::MODIFIER_ALT; }

        Self {
            scancode,
            ascii,
            modifiers,
            _reserved: 0,
        }
    }

    pub fn to_bytes(&self) -> [u8; 4] {
        [self.scancode, self.ascii, self.modifiers, self._reserved]
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 {
            return None;
        }
        Some(Self {
            scancode: bytes[0],
            ascii: bytes[1],
            modifiers: bytes[2],
            _reserved: bytes[3],
        })
    }

    pub fn shift(&self) -> bool {
        self.modifiers & Self::MODIFIER_SHIFT != 0
    }

    pub fn ctrl(&self) -> bool {
        self.modifiers & Self::MODIFIER_CTRL != 0
    }

    pub fn alt(&self) -> bool {
        self.modifiers & Self::MODIFIER_ALT != 0
    }
}

/// Mouse move event payload
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MouseMovePayload {
    /// Absolute X position (surface-relative)
    pub x: i32,
    /// Absolute Y position (surface-relative)
    pub y: i32,
    /// Delta X (relative movement)
    pub dx: i32,
    /// Delta Y (relative movement)
    pub dy: i32,
}

impl MouseMovePayload {
    pub fn new(x: i32, y: i32, dx: i32, dy: i32) -> Self {
        Self { x, y, dx, dy }
    }

    pub fn to_bytes(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&self.x.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.y.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.dx.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.dy.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 16 {
            return None;
        }
        Some(Self {
            x: i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            y: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            dx: i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            dy: i32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        })
    }
}

/// Mouse button event payload
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MouseButtonPayload {
    /// Button ID (0=left, 1=right, 2=middle)
    pub button: u8,
    /// X position at click
    pub x: i32,
    /// Y position at click
    pub y: i32,
}

impl MouseButtonPayload {
    pub const BUTTON_LEFT: u8 = 0;
    pub const BUTTON_RIGHT: u8 = 1;
    pub const BUTTON_MIDDLE: u8 = 2;

    pub fn new(button: u8, x: i32, y: i32) -> Self {
        Self { button, x, y }
    }

    pub fn to_bytes(&self) -> [u8; 12] {
        let mut bytes = [0u8; 12];
        bytes[0] = self.button;
        bytes[1..4].copy_from_slice(&[0, 0, 0]); // padding
        bytes[4..8].copy_from_slice(&self.x.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.y.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 12 {
            return None;
        }
        Some(Self {
            button: bytes[0],
            x: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            y: i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        })
    }
}

// ============================================================================
// Window/Surface Messages
// ============================================================================

/// Surface creation request payload
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SurfaceCreatePayload {
    /// Requested width
    pub width: u32,
    /// Requested height
    pub height: u32,
    /// Flags (reserved for future use)
    pub flags: u32,
}

impl SurfaceCreatePayload {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, flags: 0 }
    }

    pub fn to_bytes(&self) -> [u8; 12] {
        let mut bytes = [0u8; 12];
        bytes[0..4].copy_from_slice(&self.width.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.height.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.flags.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 12 {
            return None;
        }
        Some(Self {
            width: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            height: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            flags: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        })
    }
}

/// Surface created response payload
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SurfaceCreatedPayload {
    /// Surface ID assigned by desktop environment
    pub surface_id: u32,
    /// Actual width (may differ from requested)
    pub width: u32,
    /// Actual height (may differ from requested)
    pub height: u32,
    /// Shared memory region ID for pixel buffer
    pub buffer_region_id: u64,
    /// Buffer address (virtual address in caller's space after mapping)
    pub buffer_addr: u64,
    /// Stride in bytes
    pub stride: u32,
}

impl SurfaceCreatedPayload {
    pub fn new(
        surface_id: u32,
        width: u32,
        height: u32,
        buffer_region_id: u64,
        buffer_addr: u64,
        stride: u32,
    ) -> Self {
        Self {
            surface_id,
            width,
            height,
            buffer_region_id,
            buffer_addr,
            stride,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(&self.surface_id.to_le_bytes());
        bytes.extend_from_slice(&self.width.to_le_bytes());
        bytes.extend_from_slice(&self.height.to_le_bytes());
        bytes.extend_from_slice(&self.buffer_region_id.to_le_bytes());
        bytes.extend_from_slice(&self.buffer_addr.to_le_bytes());
        bytes.extend_from_slice(&self.stride.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 32 {
            return None;
        }
        Some(Self {
            surface_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            width: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            height: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            buffer_region_id: u64::from_le_bytes([
                bytes[12], bytes[13], bytes[14], bytes[15],
                bytes[16], bytes[17], bytes[18], bytes[19],
            ]),
            buffer_addr: u64::from_le_bytes([
                bytes[20], bytes[21], bytes[22], bytes[23],
                bytes[24], bytes[25], bytes[26], bytes[27],
            ]),
            stride: u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
        })
    }
}

/// Damage region notification (tells compositor which area changed)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DamagePayload {
    /// Surface ID
    pub surface_id: u32,
    /// Damaged region X
    pub x: u32,
    /// Damaged region Y
    pub y: u32,
    /// Damaged region width
    pub width: u32,
    /// Damaged region height
    pub height: u32,
}

impl DamagePayload {
    pub fn new(surface_id: u32, x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { surface_id, x, y, width, height }
    }

    pub fn to_bytes(&self) -> [u8; 20] {
        let mut bytes = [0u8; 20];
        bytes[0..4].copy_from_slice(&self.surface_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.x.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.y.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.width.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.height.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 20 {
            return None;
        }
        Some(Self {
            surface_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            x: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            y: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            width: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            height: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
        })
    }
}
