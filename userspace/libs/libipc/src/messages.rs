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
    /// Protocol version
    pub version: u32,
    /// Message type identifier
    pub msg_type: MessageType,
    /// Message payload size in bytes
    pub payload_size: u32,
    /// Sequence number for request/response matching
    pub sequence: u32,
}

impl MessageHeader {
    pub const SIZE: usize = core::mem::size_of::<Self>();
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new(msg_type: MessageType, payload_size: u32) -> Self {
        static SEQUENCE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        Self {
            version: Self::CURRENT_VERSION,
            msg_type,
            payload_size,
            sequence: SEQUENCE.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.version.to_le_bytes());
        bytes[4..8].copy_from_slice(&(self.msg_type as u32).to_le_bytes());
        bytes[8..12].copy_from_slice(&self.payload_size.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.sequence.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let version = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let msg_type = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let payload_size = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let sequence = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);

        Some(Self {
            version,
            msg_type: MessageType::from_u32(msg_type)?,
            payload_size,
            sequence,
        })
    }
}

// ============================================================================
// Modern Window Manager Protocol (Wm*)
// ============================================================================

/// Window Manager request types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum WmRequestType {
    CreateWindow = 1,
    DestroyWindow = 2,
    MoveWindow = 3,
    ResizeWindow = 4,
    SetTitle = 5,
}

impl WmRequestType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::CreateWindow),
            2 => Some(Self::DestroyWindow),
            3 => Some(Self::MoveWindow),
            4 => Some(Self::ResizeWindow),
            5 => Some(Self::SetTitle),
            _ => None,
        }
    }
}

/// Request to create a window
#[derive(Debug, Clone)]
pub struct WmCreateWindowRequest {
    pub reply_port: u64,
    pub width: u32,
    pub height: u32,
    pub title: String,
}

impl WmCreateWindowRequest {
    pub fn to_bytes(&self) -> Vec<u8> {
        let title_bytes = self.title.as_bytes();
        let mut bytes = Vec::with_capacity(24 + title_bytes.len());
        bytes.extend_from_slice(&(WmRequestType::CreateWindow as u32).to_le_bytes());
        bytes.extend_from_slice(&self.reply_port.to_le_bytes());
        bytes.extend_from_slice(&self.width.to_le_bytes());
        bytes.extend_from_slice(&self.height.to_le_bytes());
        bytes.extend_from_slice(&(title_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(title_bytes);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 24 { return None; }
        let reply_port = u64::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11]]);
        let width = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        let height = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let title_len = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) as usize;
        if bytes.len() < 24 + title_len { return None; }
        let title = core::str::from_utf8(&bytes[24..24 + title_len]).ok()?;
        Some(Self { reply_port, width, height, title: String::from(title) })
    }
}

/// Window Manager response with surface info
#[derive(Debug, Clone, Copy)]
pub struct WmCreateWindowResponse {
    pub window_id: WindowId,
    pub region_id: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

impl WmCreateWindowResponse {
    pub const SIZE: usize = 24;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.window_id.to_le_bytes());
        bytes[4..12].copy_from_slice(&self.region_id.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.width.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.height.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.stride.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE { return None; }
        Some(Self {
            window_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            region_id: u64::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11]]),
            width: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            height: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            stride: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        })
    }
}

/// Window event notification
#[derive(Debug, Clone, Copy)]
pub struct WmWindowEventMsg {
    pub window_id: WindowId,
    pub event_type: WindowEventType,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WmWindowEventMsg {
    pub const SIZE: usize = 21;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.window_id.to_le_bytes());
        bytes[4] = self.event_type as u8;
        bytes[5..9].copy_from_slice(&self.x.to_le_bytes());
        bytes[9..13].copy_from_slice(&self.y.to_le_bytes());
        bytes[13..17].copy_from_slice(&self.width.to_le_bytes());
        bytes[17..21].copy_from_slice(&self.height.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE { return None; }
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

/// Commit frame message
#[derive(Debug, Clone, Copy)]
pub struct WmCommitFrameMsg {
    pub window_id: WindowId,
}

impl WmCommitFrameMsg {
    pub const SIZE: usize = 4;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        self.window_id.to_le_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE { return None; }
        Some(Self {
            window_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
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
    IrqNotification = 20,

    // Window Management (100-199)
    CreateWindow = 100,
    CreateWindowResponse = 101,
    DestroyWindow = 102,
    ResizeWindow = 103,
    MoveWindow = 104,
    FocusWindow = 105,
    WindowEvent = 106,

    // Modern Window Manager Protocol (800-899)
    WmRequest = 800,
    WmResponse = 801,
    WmEvent = 802,
    WmCommitFrame = 803,

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

    // Application Lifecycle (500-599)
    /// Sent to application to provide its render surface
    SurfaceAssign = 500,
    /// Application acknowledges surface assignment
    SurfaceAck = 501,
    /// Application requests to present rendered content
    SurfacePresent = 502,
    /// Application registers its IPC port with compositor
    AppRegister = 505,
    /// Compositor requests application to terminate
    TerminateRequest = 510,
    /// Application acknowledges termination (clean exit)
    TerminateAck = 511,

    // Name Service (600-699)
    /// Register with name service
    NsRegister = 600,
    /// Unregister from name service
    NsUnregister = 601,
    /// Lookup service by name
    NsLookup = 602,
    /// List all registered services
    NsList = 603,
    /// Name service response
    NsResponse = 610,
    /// Name service error
    NsError = 611,

    // Service Manager (700-799)
    /// Start a service
    SmStartService = 700,
    /// Stop a service
    SmStopService = 701,
    /// Restart a service
    SmRestartService = 702,
    /// List all managed services
    SmListServices = 703,
    /// Get service status
    SmServiceStatus = 704,
    /// Register service IPC port
    SmRegisterPort = 705,
    /// Service manager response
    SmResponse = 710,
    /// Service manager error
    SmError = 711,


    // Block Device Protocol (1000-1099) — ahcid on PORT_BLOCK_SERVICE (10)
    /// Read sectors from block device: LBA + count + region_id for output
    BlockRead = 1000,
    /// Reply to BlockRead: bytes_read or error
    BlockReadReply = 1001,
    /// Write sectors to block device: LBA + count + region_id containing data
    BlockWrite = 1002,
    /// Reply to BlockWrite: bytes_written or error
    BlockWriteReply = 1003,
    /// Flush write cache / issue barrier
    BlockFlush = 1004,
    /// Reply to BlockFlush
    BlockFlushReply = 1005,
    /// Identify device: returns geometry (sector_count, sector_size, model)
    BlockIdentify = 1006,
    /// Reply to BlockIdentify, geometry in shared region
    BlockIdentifyReply = 1007,
    /// Discard/trim sectors (optional)
    BlockDiscard = 1008,
    /// Reply to BlockDiscard
    BlockDiscardReply = 1009,
    /// Block device error notification (asynchronous)
    BlockError = 1099,
 
    // Filesystem Protocol (1100-1199) — fsd (Filesystem Daemon) server
    /// open(path, flags, mode) -> handle
    FsOpen = 1100,
    /// Reply to FsOpen
    FsOpenReply = 1101,
    /// close(handle)
    FsClose = 1102,
    /// Reply to FsClose
    FsCloseReply = 1103,
    /// read(handle, len, region_id) -> bytes_read
    FsRead = 1104,
    /// Reply to FsRead
    FsReadReply = 1105,
    /// write(handle, len, region_id) -> bytes_written
    FsWrite = 1106,
    /// Reply to FsWrite
    FsWriteReply = 1107,
    /// lseek(handle, offset, whence) -> new_offset
    FsSeek = 1108,
    /// Reply to FsSeek
    FsSeekReply = 1109,
    /// stat(path) -> stat_struct via region
    FsStat = 1110,
    /// Reply to FsStat
    FsStatReply = 1111,
    /// fstat(handle) -> stat_struct via region
    FsFstat = 1112,
    /// Reply to FsFstat
    FsFstatReply = 1113,
    /// mkdir(path, mode)
    FsMkdir = 1114,
    /// Reply to FsMkdir
    FsMkdirReply = 1115,
    /// rmdir(path)
    FsRmdir = 1116,
    /// Reply to FsRmdir
    FsRmdirReply = 1117,
    /// unlink(path)
    FsUnlink = 1118,
    /// Reply to FsUnlink
    FsUnlinkReply = 1119,
    /// rename(old, new)
    FsRename = 1120,
    /// Reply to FsRename
    FsRenameReply = 1121,
    /// readdir(handle, max_bytes, region_id) -> bytes_filled
    FsReaddir = 1122,
    /// Reply to FsReaddir
    FsReaddirReply = 1123,
    /// truncate(handle, new_size)
    FsTruncate = 1124,
    /// Reply to FsTruncate
    FsTruncateReply = 1125,
    /// fsync(handle)
    FsFsync = 1126,
    /// Reply to FsFsync
    FsFsyncReply = 1127,
    /// mount(dev_path, mount_point, fstype, flags)
    FsMount = 1128,
    /// Reply to FsMount
    FsMountReply = 1129,
    /// umount(path)
    FsUmount = 1130,
    /// Reply to FsUmount
    FsUmountReply = 1131,
    /// chmod(path, mode)
    FsChmod = 1132,
    /// Reply to FsChmod
    FsChmodReply = 1133,
    /// link(old, new) — hard link
    FsLink = 1134,
    /// Reply to FsLink
    FsLinkReply = 1135,
    /// symlink(target, link_path)
    FsSymlink = 1136,
    /// Reply to FsSymlink
    FsSymlinkReply = 1137,
    /// readlink(path) -> target via region
    FsReadlink = 1138,
    /// Reply to FsReadlink
    FsReadlinkReply = 1139,
    /// utimes(path, atime_ns, mtime_ns)
    FsUtimes = 1140,
    /// Reply to FsUtimes
    FsUtimesReply = 1141,
    /// statvfs(path) -> statvfs_struct via region
    FsStatvfs = 1142,
    /// Reply to FsStatvfs
    FsStatvfsReply = 1143,
    /// Generic filesystem error (sent asynchronously by fsd on errors)
    FsError = 1199,
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
            20 => Some(Self::IrqNotification),
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
            500 => Some(Self::SurfaceAssign),
            501 => Some(Self::SurfaceAck),
            502 => Some(Self::SurfacePresent),
            505 => Some(Self::AppRegister),
            510 => Some(Self::TerminateRequest),
            511 => Some(Self::TerminateAck),
            // Name Service messages
            600 => Some(Self::NsRegister),
            601 => Some(Self::NsUnregister),
            602 => Some(Self::NsLookup),
            603 => Some(Self::NsList),
            610 => Some(Self::NsResponse),
            611 => Some(Self::NsError),
            // Service Manager messages
            700 => Some(Self::SmStartService),
            701 => Some(Self::SmStopService),
            702 => Some(Self::SmRestartService),
            703 => Some(Self::SmListServices),
            704 => Some(Self::SmServiceStatus),
            705 => Some(Self::SmRegisterPort),
            710 => Some(Self::SmResponse),
            711 => Some(Self::SmError),
            800 => Some(Self::WmRequest),
            801 => Some(Self::WmResponse),
            802 => Some(Self::WmEvent),
            803 => Some(Self::WmCommitFrame),
            1000 => Some(Self::BlockRead),
            //Block Device Protocol messages
            1001 => Some(Self::BlockReadReply),
            1002 => Some(Self::BlockWrite),
            1003 => Some(Self::BlockWriteReply),
            1004 => Some(Self::BlockFlush),
            1005 => Some(Self::BlockFlushReply),
            1006 => Some(Self::BlockIdentify),
            1007 => Some(Self::BlockIdentifyReply),
            1008 => Some(Self::BlockDiscard),
            1009 => Some(Self::BlockDiscardReply),
            1099 => Some(Self::BlockError),
            // Filesystem Protocol
            1100 => Some(Self::FsOpen),
            1101 => Some(Self::FsOpenReply),
            1102 => Some(Self::FsClose),
            1103 => Some(Self::FsCloseReply),
            1104 => Some(Self::FsRead),
            1105 => Some(Self::FsReadReply),
            1106 => Some(Self::FsWrite),
            1107 => Some(Self::FsWriteReply),
            1108 => Some(Self::FsSeek),
            1109 => Some(Self::FsSeekReply),
            1110 => Some(Self::FsStat),
            1111 => Some(Self::FsStatReply),
            1112 => Some(Self::FsFstat),
            1113 => Some(Self::FsFstatReply),
            1114 => Some(Self::FsMkdir),
            1115 => Some(Self::FsMkdirReply),
            1116 => Some(Self::FsRmdir),
            1117 => Some(Self::FsRmdirReply),
            1118 => Some(Self::FsUnlink),
            1119 => Some(Self::FsUnlinkReply),
            1120 => Some(Self::FsRename),
            1121 => Some(Self::FsRenameReply),
            1122 => Some(Self::FsReaddir),
            1123 => Some(Self::FsReaddirReply),
            1124 => Some(Self::FsTruncate),
            1125 => Some(Self::FsTruncateReply),
            1126 => Some(Self::FsFsync),
            1127 => Some(Self::FsFsyncReply),
            1128 => Some(Self::FsMount),
            1129 => Some(Self::FsMountReply),
            1130 => Some(Self::FsUmount),
            1131 => Some(Self::FsUmountReply),
            1132 => Some(Self::FsChmod),
            1133 => Some(Self::FsChmodReply),
            1134 => Some(Self::FsLink),
            1135 => Some(Self::FsLinkReply),
            1136 => Some(Self::FsSymlink),
            1137 => Some(Self::FsSymlinkReply),
            1138 => Some(Self::FsReadlink),
            1139 => Some(Self::FsReadlinkReply),
            1140 => Some(Self::FsUtimes),
            1141 => Some(Self::FsUtimesReply),
            1142 => Some(Self::FsStatvfs),
            1143 => Some(Self::FsStatvfsReply),
            1199 => Some(Self::FsError),
            _ => None,
        }
    }
}

// ============================================================================
// Application Lifecycle Messages
// ============================================================================

/// Surface assignment message sent from compositor to application
/// Contains shared surface information for the application to render into
#[derive(Debug, Clone, Copy)]
pub struct SurfaceAssignMsg {
    /// Window ID that owns this surface
    pub window_id: WindowId,
    /// Shared memory region ID
    pub region_id: u64,
    /// Surface dimensions
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bytes_per_pixel: u32,
    /// IPC port to send present requests back to compositor
    pub compositor_port: u64,
    /// DPI Scale factor (scaled by 1000, e.g. 1000 = 1.0)
    pub scale_factor: u32,
}

impl SurfaceAssignMsg {
    pub const SIZE: usize = 40; // 4 + 8 + 4 + 4 + 4 + 4 + 8 + 4

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.window_id.to_le_bytes());
        bytes[4..12].copy_from_slice(&self.region_id.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.width.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.height.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.stride.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.bytes_per_pixel.to_le_bytes());
        bytes[28..36].copy_from_slice(&self.compositor_port.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.scale_factor.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            window_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            region_id: u64::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11]]),
            width: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            height: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            stride: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            bytes_per_pixel: u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            compositor_port: u64::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31], bytes[32], bytes[33], bytes[34], bytes[35]]),
            scale_factor: u32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]),
        })
    }
}

/// Terminate request sent from compositor to application
#[derive(Debug, Clone, Copy)]
pub struct TerminateRequestMsg {
    pub window_id: WindowId,
    pub reason: u32, // 0 = user requested close, 1 = system shutdown
}

impl TerminateRequestMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.window_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.reason.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            window_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            reason: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }
}

/// Surface present message sent from application to compositor
/// Notifies the compositor that the application has finished rendering
/// and the surface content should be composited to the screen
#[derive(Debug, Clone, Copy)]
pub struct SurfacePresentMsg {
    /// Window ID that owns this surface
    pub window_id: u32,
}

impl SurfacePresentMsg {
    pub const SIZE: usize = 4;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        self.window_id.to_le_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            window_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        })
    }
}

/// Application registration message sent from app to compositor
/// This tells the compositor which port to send window events to
#[derive(Debug, Clone, Copy)]
pub struct AppRegisterMsg {
    /// The application's IPC port for receiving messages
    pub app_port: u64,
    /// Process ID (for matching to pending windows)
    pub pid: u64,
}

impl AppRegisterMsg {
    pub const SIZE: usize = 16;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.app_port.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.pid.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            app_port: u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]),
            pid: u64::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]]),
        })
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
    Button4 = 3,
    Button5 = 4,
}

impl MouseButton {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Left),
            1 => Some(Self::Right),
            2 => Some(Self::Middle),
            3 => Some(Self::Button4),
            4 => Some(Self::Button5),
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

/// Mouse scroll event (wheel movement)
///
/// dz values from the PS/2 spec:
///   +1 = scroll up (vertical) or right (horizontal)
///   -1 = scroll down (vertical) or left (horizontal)
///   Larger magnitudes indicate faster scrolling.
#[derive(Debug, Clone, Copy)]
pub struct MouseScrollEvent {
    /// Vertical scroll delta (positive = up, negative = down)
    pub dz: i32,
    /// Current cursor X position
    pub x: i32,
    /// Current cursor Y position
    pub y: i32,
}

impl MouseScrollEvent {
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut bytes = [0u8; 12];
        bytes[0..4].copy_from_slice(&self.dz.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.x.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.y.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 12 {
            return None;
        }
        Some(Self {
            dz: i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            x: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            y: i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
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

// ============================================================================
// Block Device Protocol (IPC wire format)
//
// All block IPC messages use the following layout:
//
//   Common header (12 bytes):
//     [0..4]   flags/pad: u32
//     [4..12]  reply_port: u64   (kernel fills this; userspace may set it)
//
//   Then operation-specific payload follows.
//
// Replies always start with:
//     [0..8]   error: u64   (0 = success, non-zero = ABI error code)
//     [8..16]  value: u64   (bytes read/written, or other return value)
// ============================================================================
 
/// BlockRead request payload (after common 12-byte header).
/// Total: 12 + 24 = 36 bytes.
#[derive(Debug, Clone, Copy)]
pub struct BlockReadReq {
    /// Starting logical block address (512-byte sectors).
    pub lba: u64,
    /// Number of sectors to read.
    pub sector_count: u32,
    /// Shared memory region ID for output data.
    pub region_id: u64,
    _pad: u32,
}
 
impl BlockReadReq {
    pub const SIZE: usize = 24;
 
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..8].copy_from_slice(&self.lba.to_le_bytes());
        b[8..12].copy_from_slice(&self.sector_count.to_le_bytes());
        b[12..20].copy_from_slice(&self.region_id.to_le_bytes());
        b[20..24].copy_from_slice(&self._pad.to_le_bytes());
        b
    }
 
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE { return None; }
        Some(Self {
            lba: u64::from_le_bytes(b[0..8].try_into().ok()?),
            sector_count: u32::from_le_bytes(b[8..12].try_into().ok()?),
            region_id: u64::from_le_bytes(b[12..20].try_into().ok()?),
            _pad: u32::from_le_bytes(b[20..24].try_into().ok()?),
        })
    }
}
 
/// BlockWrite request payload (same layout as BlockRead).
pub type BlockWriteReq = BlockReadReq;
 
/// Identify reply payload (written to shared region, 256 bytes).
#[derive(Debug, Clone, Copy)]
pub struct BlockIdentifyInfo {
    /// Total number of 512-byte sectors.
    pub total_sectors: u64,
    /// Bytes per sector (usually 512 or 4096).
    pub sector_size: u32,
    /// Optimal transfer size in sectors.
    pub optimal_xfer: u32,
    /// Device model string (40 bytes, space-padded).
    pub model: [u8; 40],
    /// Serial number (20 bytes, space-padded).
    pub serial: [u8; 20],
    /// Firmware revision (8 bytes, space-padded).
    pub firmware: [u8; 8],
    /// Non-zero if device supports TRIM/discard.
    pub supports_trim: u8,
    /// Non-zero if device is read-only.
    pub read_only: u8,
    _pad: [u8; 126],
}
 
impl BlockIdentifyInfo {
    pub const SIZE: usize = 256;
 
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..8].copy_from_slice(&self.total_sectors.to_le_bytes());
        b[8..12].copy_from_slice(&self.sector_size.to_le_bytes());
        b[12..16].copy_from_slice(&self.optimal_xfer.to_le_bytes());
        b[16..56].copy_from_slice(&self.model);
        b[56..76].copy_from_slice(&self.serial);
        b[76..84].copy_from_slice(&self.firmware);
        b[84] = self.supports_trim;
        b[85] = self.read_only;
        b
    }
 
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE { return None; }
        let mut info = Self {
            total_sectors: u64::from_le_bytes(b[0..8].try_into().ok()?),
            sector_size: u32::from_le_bytes(b[8..12].try_into().ok()?),
            optimal_xfer: u32::from_le_bytes(b[12..16].try_into().ok()?),
            model: [0u8; 40],
            serial: [0u8; 20],
            firmware: [0u8; 8],
            supports_trim: b[84],
            read_only: b[85],
            _pad: [0u8; 126],
        };
        info.model.copy_from_slice(&b[16..56]);
        info.serial.copy_from_slice(&b[56..76]);
        info.firmware.copy_from_slice(&b[76..84]);
        Some(info)
    }
}
 
// ============================================================================
// Filesystem Protocol — reply wire format
//
// Every fsd reply starts with 16 bytes:
//     [0..8]   error: u64   (0 = ESUCCESS)
//     [8..16]  value: u64   (fd, bytes_read, new_offset, etc.)
//
// Larger data (stat structs, dir entries, file data) go in a shared region
// whose ID was sent in the request.
// ============================================================================
 
/// Generic FS reply (16 bytes).
#[derive(Debug, Clone, Copy)]
pub struct FsReply {
    pub error: u64,
    pub value: u64,
}
 
impl FsReply {
    pub const SIZE: usize = 16;
    pub const SUCCESS: Self = Self { error: 0, value: 0 };
 
    pub fn ok(value: u64) -> Self { Self { error: 0, value } }
    pub fn err(code: u64) -> Self { Self { error: code, value: 0 } }
 
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..8].copy_from_slice(&self.error.to_le_bytes());
        b[8..16].copy_from_slice(&self.value.to_le_bytes());
        b
    }
 
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE { return None; }
        Some(Self {
            error: u64::from_le_bytes(b[0..8].try_into().ok()?),
            value: u64::from_le_bytes(b[8..16].try_into().ok()?),
        })
    }
 
    pub fn is_ok(&self) -> bool { self.error == 0 }
}
 
/// On-wire stat structure (80 bytes) written into shared region by fsd.
/// Layout is fixed and shared between kernel, fsd, and userspace programs.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsStatBuf {
    pub size:     u64,    // file size in bytes
    pub inode:    u64,    // inode number
    pub mtime_ns: u64,    // modification time (nanoseconds since epoch)
    pub atime_ns: u64,    // access time
    pub ctime_ns: u64,    // change time (metadata)
    pub mode:     u32,    // type + permissions (POSIX-style)
    pub uid:      u16,
    pub gid:      u16,
    pub nlinks:   u32,    // hard link count
    pub nblocks:  u32,    // 512-byte blocks allocated
    pub blksize:  u32,    // preferred I/O block size
    pub dev:      u32,    // device ID (mount number)
    _reserved:    [u8; 16],
}
 
impl FsStatBuf {
    pub const SIZE: usize = 80;
 
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..8].copy_from_slice(&self.size.to_le_bytes());
        b[8..16].copy_from_slice(&self.inode.to_le_bytes());
        b[16..24].copy_from_slice(&self.mtime_ns.to_le_bytes());
        b[24..32].copy_from_slice(&self.atime_ns.to_le_bytes());
        b[32..40].copy_from_slice(&self.ctime_ns.to_le_bytes());
        b[40..44].copy_from_slice(&self.mode.to_le_bytes());
        b[44..46].copy_from_slice(&self.uid.to_le_bytes());
        b[46..48].copy_from_slice(&self.gid.to_le_bytes());
        b[48..52].copy_from_slice(&self.nlinks.to_le_bytes());
        b[52..56].copy_from_slice(&self.nblocks.to_le_bytes());
        b[56..60].copy_from_slice(&self.blksize.to_le_bytes());
        b[60..64].copy_from_slice(&self.dev.to_le_bytes());
        b
    }
 
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE { return None; }
        Some(Self {
            size:     u64::from_le_bytes(b[0..8].try_into().ok()?),
            inode:    u64::from_le_bytes(b[8..16].try_into().ok()?),
            mtime_ns: u64::from_le_bytes(b[16..24].try_into().ok()?),
            atime_ns: u64::from_le_bytes(b[24..32].try_into().ok()?),
            ctime_ns: u64::from_le_bytes(b[32..40].try_into().ok()?),
            mode:     u32::from_le_bytes(b[40..44].try_into().ok()?),
            uid:      u16::from_le_bytes(b[44..46].try_into().ok()?),
            gid:      u16::from_le_bytes(b[46..48].try_into().ok()?),
            nlinks:   u32::from_le_bytes(b[48..52].try_into().ok()?),
            nblocks:  u32::from_le_bytes(b[52..56].try_into().ok()?),
            blksize:  u32::from_le_bytes(b[56..60].try_into().ok()?),
            dev:      u32::from_le_bytes(b[60..64].try_into().ok()?),
            _reserved: [0u8; 16],
        })
    }
}
 
/// On-wire statvfs structure (72 bytes) for filesystem statistics.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsStatvfsBuf {
    pub bsize:    u64,    // filesystem block size
    pub frsize:   u64,    // fundamental block size (for f_blocks, f_bfree, f_bavail)
    pub blocks:   u64,    // total data blocks
    pub bfree:    u64,    // free blocks
    pub bavail:   u64,    // free blocks available to non-root
    pub files:    u64,    // total inodes
    pub ffree:    u64,    // free inodes
    pub favail:   u64,    // free inodes for non-root
    pub namemax:  u32,    // max filename length
    pub flags:    u32,    // mount flags (ST_RDONLY etc.)
}
 
impl FsStatvfsBuf {
    pub const SIZE: usize = 72;
 
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..8].copy_from_slice(&self.bsize.to_le_bytes());
        b[8..16].copy_from_slice(&self.frsize.to_le_bytes());
        b[16..24].copy_from_slice(&self.blocks.to_le_bytes());
        b[24..32].copy_from_slice(&self.bfree.to_le_bytes());
        b[32..40].copy_from_slice(&self.bavail.to_le_bytes());
        b[40..48].copy_from_slice(&self.files.to_le_bytes());
        b[48..56].copy_from_slice(&self.ffree.to_le_bytes());
        b[56..64].copy_from_slice(&self.favail.to_le_bytes());
        b[64..68].copy_from_slice(&self.namemax.to_le_bytes());
        b[68..72].copy_from_slice(&self.flags.to_le_bytes());
        b
    }
 
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE { return None; }
        Some(Self {
            bsize:   u64::from_le_bytes(b[0..8].try_into().ok()?),
            frsize:  u64::from_le_bytes(b[8..16].try_into().ok()?),
            blocks:  u64::from_le_bytes(b[16..24].try_into().ok()?),
            bfree:   u64::from_le_bytes(b[24..32].try_into().ok()?),
            bavail:  u64::from_le_bytes(b[32..40].try_into().ok()?),
            files:   u64::from_le_bytes(b[40..48].try_into().ok()?),
            ffree:   u64::from_le_bytes(b[48..56].try_into().ok()?),
            favail:  u64::from_le_bytes(b[56..64].try_into().ok()?),
            namemax: u32::from_le_bytes(b[64..68].try_into().ok()?),
            flags:   u32::from_le_bytes(b[68..72].try_into().ok()?),
        })
    }
}
 
/// On-wire directory entry (variable length, serialized in a shared region).
/// Multiple entries packed contiguously; rec_len must be 4-byte aligned.
///
/// Layout:
///   [0..4]   ino: u32
///   [4..6]   rec_len: u16   (total length of this record, 4-byte aligned)
///   [6]      name_len: u8
///   [7]      file_type: u8  (0=unknown, 1=regular, 2=dir, 3=symlink,
///                            4=block, 5=char, 6=fifo, 7=socket)
///   [8..]    name bytes (not NUL-terminated)
pub struct FsDirentIter<'a> {
    data: &'a [u8],
    pos:  usize,
}
 
impl<'a> FsDirentIter<'a> {
    pub fn new(data: &'a [u8]) -> Self { Self { data, pos: 0 } }
}
 
#[derive(Debug, Clone)]
pub struct FsDirentEntry {
    pub ino:       u32,
    pub file_type: u8,
    pub name:      alloc::string::String,
}
 
impl<'a> Iterator for FsDirentIter<'a> {
    type Item = FsDirentEntry;
 
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 8 > self.data.len() { return None; }
        let b = &self.data[self.pos..];
        let ino = u32::from_le_bytes(b[0..4].try_into().ok()?);
        let rec_len = u16::from_le_bytes(b[4..6].try_into().ok()?) as usize;
        let name_len = b[6] as usize;
        let file_type = b[7];
 
        if rec_len < 8 || rec_len % 4 != 0 || self.pos + rec_len > self.data.len() {
            return None;
        }
        if 8 + name_len > rec_len { return None; }
 
        let name = String::from(core::str::from_utf8(&b[8..8 + name_len])
            .unwrap_or(""));
 
        self.pos += rec_len;
        if ino == 0 { return self.next(); } // skip deleted entries
 
        Some(FsDirentEntry { ino, file_type, name })
    }
}
 
/// Serialize a directory entry into a Vec for writing into a shared region.
pub fn serialize_fs_dirent(ino: u32, file_type: u8, name: &str) -> alloc::vec::Vec<u8> {
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(255);
    let raw_len = 8 + name_len;
    let rec_len = (raw_len + 3) & !3; // round up to 4-byte boundary
    let mut v = alloc::vec![0u8; rec_len];
    v[0..4].copy_from_slice(&ino.to_le_bytes());
    v[4..6].copy_from_slice(&(rec_len as u16).to_le_bytes());
    v[6] = name_len as u8;
    v[7] = file_type;
    v[8..8 + name_len].copy_from_slice(&name_bytes[..name_len]);
    v
}

// ============================================================================
// Name Service Messages
// ============================================================================

/// Message to register a service with the name service
#[derive(Debug, Clone, Copy)]
pub struct NsRegisterMsg {
    /// The port to register for this service
    pub port: u64,
    /// The name of the service (null-terminated or fixed-size)
    pub name: [u8; 32],
}

impl NsRegisterMsg {
    pub const SIZE: usize = 8 + 32;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.port.to_le_bytes());
        bytes[8..40].copy_from_slice(&self.name);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let port = u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]);
        let mut name = [0u8; 32];
        name.copy_from_slice(&bytes[8..40]);
        Some(Self { port, name })
    }
}

/// Message to lookup a service by name
#[derive(Debug, Clone, Copy)]
pub struct NsLookupMsg {
    /// The port to send the response back to
    pub reply_port: u64,
    /// The name of the service to lookup
    pub name: [u8; 32],
}

impl NsLookupMsg {
    pub const SIZE: usize = 8 + 32;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8..40].copy_from_slice(&self.name);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let reply_port = u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]);
        let mut name = [0u8; 32];
        name.copy_from_slice(&bytes[8..40]);
        Some(Self { reply_port, name })
    }
}

/// Response from the name service
#[derive(Debug, Clone, Copy)]
pub struct NsResponseMsg {
    /// The port of the requested service (0 if not found)
    pub port: u64,
}

impl NsResponseMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        self.port.to_le_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            port: u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }
}
