//! IPC Message Definitions
//!
//! This module defines all message types used for communication between
//! userspace components in the Atom desktop environment.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ============================================================================
// Message Header
// ============================================================================

/// Common header for all IPC messages
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MessageHeader {
    /// Message type identifier
    pub msg_type: MessageType,
    /// Message payload size in bytes
    pub payload_size: u32,
    /// Sequence number for request/response matching
    pub sequence: u32,
}

impl MessageHeader {
    pub const SIZE: usize = core::mem::size_of::<Self>();

    pub fn new(msg_type: MessageType, payload_size: u32) -> Self {
        static SEQUENCE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        Self {
            msg_type,
            payload_size,
            sequence: SEQUENCE.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&(self.msg_type as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(&self.payload_size.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.sequence.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let msg_type = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let payload_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let sequence = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);

        Some(Self {
            msg_type: MessageType::from_u32(msg_type)?,
            payload_size,
            sequence,
        })
    }
}

// ============================================================================
// Message Types
// ============================================================================

/// All possible message types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MessageType {
    // Input Events (1-99)
    KeyDown = 1,
    KeyUp = 2,
    KeyPress = 3,  // Key with character
    MouseMove = 10,
    MouseButtonDown = 11,
    MouseButtonUp = 12,
    MouseScroll = 13,

    // Window Management (100-199)
    CreateWindow = 100,
    CreateWindowResponse = 101,
    DestroyWindow = 102,
    ResizeWindow = 103,
    MoveWindow = 104,
    FocusWindow = 105,
    WindowEvent = 106,

    // Graphics (200-299)
    GetFramebuffer = 200,
    FramebufferInfo = 201,
    InvalidateRect = 202,
    Present = 203,
    CreateSurface = 210,
    DestroySurface = 211,
    BlitSurface = 212,

    // Service Discovery (300-399)
    RegisterService = 300,
    LookupService = 301,
    ServiceInfo = 302,

    // System (400-499)
    Ping = 400,
    Pong = 401,
    Shutdown = 402,
    Error = 499,
}

impl MessageType {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::KeyDown),
            2 => Some(Self::KeyUp),
            3 => Some(Self::KeyPress),
            10 => Some(Self::MouseMove),
            11 => Some(Self::MouseButtonDown),
            12 => Some(Self::MouseButtonUp),
            13 => Some(Self::MouseScroll),
            100 => Some(Self::CreateWindow),
            101 => Some(Self::CreateWindowResponse),
            102 => Some(Self::DestroyWindow),
            103 => Some(Self::ResizeWindow),
            104 => Some(Self::MoveWindow),
            105 => Some(Self::FocusWindow),
            106 => Some(Self::WindowEvent),
            200 => Some(Self::GetFramebuffer),
            201 => Some(Self::FramebufferInfo),
            202 => Some(Self::InvalidateRect),
            203 => Some(Self::Present),
            210 => Some(Self::CreateSurface),
            211 => Some(Self::DestroySurface),
            212 => Some(Self::BlitSurface),
            300 => Some(Self::RegisterService),
            301 => Some(Self::LookupService),
            302 => Some(Self::ServiceInfo),
            400 => Some(Self::Ping),
            401 => Some(Self::Pong),
            402 => Some(Self::Shutdown),
            499 => Some(Self::Error),
            _ => None,
        }
    }
}

// ============================================================================
// Input Event Messages
// ============================================================================

/// Key modifier flags
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub caps_lock: bool,
}

impl KeyModifiers {
    pub fn to_u8(&self) -> u8 {
        let mut flags = 0u8;
        if self.shift { flags |= 0x01; }
        if self.ctrl { flags |= 0x02; }
        if self.alt { flags |= 0x04; }
        if self.caps_lock { flags |= 0x08; }
        flags
    }

    pub fn from_u8(flags: u8) -> Self {
        Self {
            shift: flags & 0x01 != 0,
            ctrl: flags & 0x02 != 0,
            alt: flags & 0x04 != 0,
            caps_lock: flags & 0x08 != 0,
        }
    }
}

/// Keyboard event
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    /// Scancode from hardware
    pub scancode: u8,
    /// ASCII character (if applicable)
    pub character: u8,
    /// Key modifiers
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    pub fn to_bytes(&self) -> [u8; 3] {
        [self.scancode, self.character, self.modifiers.to_u8()]
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 3 {
            return None;
        }
        Some(Self {
            scancode: bytes[0],
            character: bytes[1],
            modifiers: KeyModifiers::from_u8(bytes[2]),
        })
    }
}

/// Mouse button identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MouseButton {
    Left = 0,
    Right = 1,
    Middle = 2,
}

impl MouseButton {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Left),
            1 => Some(Self::Right),
            2 => Some(Self::Middle),
            _ => None,
        }
    }
}

/// Mouse move event
#[derive(Debug, Clone, Copy)]
pub struct MouseMoveEvent {
    /// Absolute X position
    pub x: i32,
    /// Absolute Y position
    pub y: i32,
    /// Delta X (relative movement)
    pub dx: i16,
    /// Delta Y (relative movement)
    pub dy: i16,
}

impl MouseMoveEvent {
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut bytes = [0u8; 12];
        bytes[0..4].copy_from_slice(&self.x.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.y.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.dx.to_le_bytes());
        bytes[10..12].copy_from_slice(&self.dy.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 12 {
            return None;
        }
        Some(Self {
            x: i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            y: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            dx: i16::from_le_bytes([bytes[8], bytes[9]]),
            dy: i16::from_le_bytes([bytes[10], bytes[11]]),
        })
    }
}

/// Mouse button event
#[derive(Debug, Clone, Copy)]
pub struct MouseButtonEvent {
    pub button: MouseButton,
    pub x: i32,
    pub y: i32,
}

impl MouseButtonEvent {
    pub fn to_bytes(&self) -> [u8; 9] {
        let mut bytes = [0u8; 9];
        bytes[0] = self.button as u8;
        bytes[1..5].copy_from_slice(&self.x.to_le_bytes());
        bytes[5..9].copy_from_slice(&self.y.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 9 {
            return None;
        }
        Some(Self {
            button: MouseButton::from_u8(bytes[0])?,
            x: i32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]),
            y: i32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
        })
    }
}

// ============================================================================
// Window Management Messages
// ============================================================================

/// Window handle (assigned by desktop compositor)
pub type WindowId = u32;

/// Request to create a new window
#[derive(Debug, Clone)]
pub struct CreateWindowRequest {
    pub width: u32,
    pub height: u32,
    pub title: String,
}

impl CreateWindowRequest {
    pub fn to_bytes(&self) -> Vec<u8> {
        let title_bytes = self.title.as_bytes();
        let mut bytes = Vec::with_capacity(12 + title_bytes.len());
        bytes.extend_from_slice(&self.width.to_le_bytes());
        bytes.extend_from_slice(&self.height.to_le_bytes());
        bytes.extend_from_slice(&(title_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(title_bytes);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 12 {
            return None;
        }
        let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let title_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;

        if bytes.len() < 12 + title_len {
            return None;
        }

        let title = core::str::from_utf8(&bytes[12..12 + title_len]).ok()?;

        Some(Self {
            width,
            height,
            title: String::from(title),
        })
    }
}

/// Response to create window request
#[derive(Debug, Clone, Copy)]
pub struct CreateWindowResponse {
    pub window_id: WindowId,
    pub success: bool,
}

impl CreateWindowResponse {
    pub fn to_bytes(&self) -> [u8; 5] {
        let mut bytes = [0u8; 5];
        bytes[0..4].copy_from_slice(&self.window_id.to_le_bytes());
        bytes[4] = if self.success { 1 } else { 0 };
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 5 {
            return None;
        }
        Some(Self {
            window_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            success: bytes[4] != 0,
        })
    }
}

/// Window event types sent to applications
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WindowEventType {
    Resize = 1,
    Move = 2,
    Focus = 3,
    Unfocus = 4,
    Close = 5,
    Expose = 6,  // Area needs redraw
}

impl WindowEventType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Resize),
            2 => Some(Self::Move),
            3 => Some(Self::Focus),
            4 => Some(Self::Unfocus),
            5 => Some(Self::Close),
            6 => Some(Self::Expose),
            _ => None,
        }
    }
}

/// Window event notification
#[derive(Debug, Clone, Copy)]
pub struct WindowEventMsg {
    pub window_id: WindowId,
    pub event_type: WindowEventType,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowEventMsg {
    pub fn to_bytes(&self) -> [u8; 21] {
        let mut bytes = [0u8; 21];
        bytes[0..4].copy_from_slice(&self.window_id.to_le_bytes());
        bytes[4] = self.event_type as u8;
        bytes[5..9].copy_from_slice(&self.x.to_le_bytes());
        bytes[9..13].copy_from_slice(&self.y.to_le_bytes());
        bytes[13..17].copy_from_slice(&self.width.to_le_bytes());
        bytes[17..21].copy_from_slice(&self.height.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 21 {
            return None;
        }
        Some(Self {
            window_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            event_type: WindowEventType::from_u8(bytes[4])?,
            x: i32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
            y: i32::from_le_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]),
            width: u32::from_le_bytes([bytes[13], bytes[14], bytes[15], bytes[16]]),
            height: u32::from_le_bytes([bytes[17], bytes[18], bytes[19], bytes[20]]),
        })
    }
}

// ============================================================================
// Graphics Messages
// ============================================================================

/// Framebuffer information
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bytes_per_pixel: u32,
    pub size: u64,
}

impl FramebufferInfo {
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0..8].copy_from_slice(&self.address.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.width.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.height.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.stride.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.bytes_per_pixel.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.size.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 32 {
            return None;
        }
        Some(Self {
            address: u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]),
            width: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            height: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            stride: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            bytes_per_pixel: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            size: u64::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30], bytes[31]]),
        })
    }
}

/// Rectangle for damage/invalidation
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    pub fn to_bytes(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&self.x.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.y.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.width.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.height.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 16 {
            return None;
        }
        Some(Self {
            x: i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            y: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            width: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            height: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        })
    }
}
