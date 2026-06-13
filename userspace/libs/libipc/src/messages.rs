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
        if bytes.len() < 24 {
            return None;
        }
        let reply_port = u64::from_le_bytes([
            bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
        ]);
        let width = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        let height = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let title_len = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) as usize;
        if bytes.len() < 24 + title_len {
            return None;
        }
        let title = core::str::from_utf8(&bytes[24..24 + title_len]).ok()?;
        Some(Self {
            reply_port,
            width,
            height,
            title: String::from(title),
        })
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
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            window_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            region_id: u64::from_le_bytes([
                bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
            ]),
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
        if bytes.len() < Self::SIZE {
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
        if bytes.len() < Self::SIZE {
            return None;
        }
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
    KeyPress = 3, // Key with character
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
    /// Sent by a process to the compositor after a successful SYS_SET_VIDEO_MODE.
    /// The compositor must re-acquire the framebuffer and rebuild its backbuffer.
    VideoModeChanged = 204,
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

    // App Launcher Protocol (1200-1299)
    // ──────────────────────────────────────────────────────────────────────
    // The app_launcher service listens on a named port registered as
    // "app_launcher" with the name service.
    //
    // Protocol version: 1 (field in AppLaunchRequestMsg; the launcher
    // rejects unknown versions with status=99 so the ABI can evolve).
    //
    // Flow:
    //   Sender → launcher : AppLaunchRequest (contains reply_port + path)
    //   Launcher → sender : AppLaunchReply   (sent to reply_port)
    //
    // AppLaunchReplyMsg.status codes:
    //   LAUNCH_OK           = 0   launch succeeded; pid field is valid
    //   LAUNCH_ERR_NOTFOUND = 1   .atxf file not found on filesystem
    //   LAUNCH_ERR_INVALID  = 2   file is not a valid ATXF image
    //   LAUNCH_ERR_NOMEM    = 3   out of physical memory
    //   LAUNCH_ERR_BADPATH  = 4   path is empty, too long, or wrong extension
    //   LAUNCH_ERR_BADTYPE  = 5   path does not end in ".atxf" (wrong file type)
    //   LAUNCH_ERR_NOFS     = 6   filesystem service unavailable (FAT32 not ready)
    //   LAUNCH_ERR_INTERNAL = 99  unspecified launcher-internal error
    // ──────────────────────────────────────────────────────────────────────
    /// Request the app_launcher to start an ATXF application by path.
    AppLaunchRequest = 1200,
    /// Response from app_launcher back to the requesting process.
    AppLaunchReply = 1201,

    // Display Settings Protocol (1300-1399)
    /// Request to open Display Settings in a specific tab
    OpenInTab = 1300,
    /// Request to apply wallpaper configuration
    ApplyWallpaper = 1301,
    /// Notification that wallpaper was applied successfully
    WallpaperApplied = 1302,
    /// Notification that wallpaper application failed
    WallpaperFailed = 1303,

    // Networking (1400-1499)
    /// netd -> nic_driver: Here is the shared memory region for rings
    NetAssignRings = 1400,
    /// nic_driver -> netd: I'm ready (includes MAC address)
    NetDriverReady = 1401,
    /// init -> netd: configure IP/mask/gateway/DNS
    NetConfigure = 1402,
    /// app -> netd: create socket
    NetSocket = 1410,
    /// netd -> app: socket creation reply
    NetSocketReply = 1411,
    /// app -> netd: connect TCP socket
    NetConnect = 1412,
    /// netd -> app: connect reply
    NetConnectReply = 1413,
    /// app -> netd: send data
    NetSend = 1414,
    /// netd -> app: send reply
    NetSendReply = 1415,
    /// app -> netd: receive data
    NetRecv = 1416,
    /// netd -> app: receive reply
    NetRecvReply = 1417,
    /// app -> netd: close socket
    NetClose = 1418,
    /// netd -> app: close reply
    NetCloseReply = 1419,
    /// app -> netd: resolve hostname
    NetResolve = 1420,
    /// netd -> app: resolve reply
    NetResolveReply = 1421,
    /// app -> netd: ICMP Echo Request
    NetIcmpEchoRequest = 1422,
    /// netd -> app: ICMP Echo Reply
    NetIcmpEchoReply = 1423,
    /// app -> netd: get current network config
    NetGetConfig = 1424,
    /// netd -> app: network config reply
    NetGetConfigReply = 1425,

    // Date and Time Service (1500-1599)
    /// Client -> timesync: request the current clock and configuration state
    TimeGetState = 1500,
    /// timesync -> client: current clock and configuration state
    TimeStateReply = 1501,
    /// Client -> timesync: update locale, time zone, format, or automatic sync
    TimeSetConfig = 1502,
    /// Client -> timesync: request an immediate internet synchronization
    TimeSyncNow = 1503,

    // Audio Service (1600-1699)
    /// Client -> audiod: request current volume, mute, and device state
    AudioGetState = 1600,
    /// audiod -> client: current audio state
    AudioStateReply = 1601,
    /// Client -> audiod: update the global output volume and mute state
    AudioSetState = 1602,
    /// Client -> audiod: play a system PCM WAV asset
    AudioPlayFile = 1603,
    /// Client -> audiod: stop any audio currently playing
    AudioStop = 1604,
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
            204 => Some(Self::VideoModeChanged),
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
            // App Launcher Protocol
            1200 => Some(Self::AppLaunchRequest),
            1201 => Some(Self::AppLaunchReply),
            // Display Settings Protocol
            1300 => Some(Self::OpenInTab),
            1301 => Some(Self::ApplyWallpaper),
            1302 => Some(Self::WallpaperApplied),
            1303 => Some(Self::WallpaperFailed),
            1400 => Some(Self::NetAssignRings),
            1401 => Some(Self::NetDriverReady),
            1402 => Some(Self::NetConfigure),
            1410 => Some(Self::NetSocket),
            1411 => Some(Self::NetSocketReply),
            1412 => Some(Self::NetConnect),
            1413 => Some(Self::NetConnectReply),
            1414 => Some(Self::NetSend),
            1415 => Some(Self::NetSendReply),
            1416 => Some(Self::NetRecv),
            1417 => Some(Self::NetRecvReply),
            1418 => Some(Self::NetClose),
            1419 => Some(Self::NetCloseReply),
            1420 => Some(Self::NetResolve),
            1421 => Some(Self::NetResolveReply),
            1422 => Some(Self::NetIcmpEchoRequest),
            1423 => Some(Self::NetIcmpEchoReply),
            1424 => Some(Self::NetGetConfig),
            1425 => Some(Self::NetGetConfigReply),
            1500 => Some(Self::TimeGetState),
            1501 => Some(Self::TimeStateReply),
            1502 => Some(Self::TimeSetConfig),
            1503 => Some(Self::TimeSyncNow),
            1600 => Some(Self::AudioGetState),
            1601 => Some(Self::AudioStateReply),
            1602 => Some(Self::AudioSetState),
            1603 => Some(Self::AudioPlayFile),
            1604 => Some(Self::AudioStop),
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
            region_id: u64::from_le_bytes([
                bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
            ]),
            width: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            height: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            stride: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            bytes_per_pixel: u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            compositor_port: u64::from_le_bytes([
                bytes[28], bytes[29], bytes[30], bytes[31], bytes[32], bytes[33], bytes[34],
                bytes[35],
            ]),
            scale_factor: u32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]),
        })
    }
}

/// app -> netd: get current network config
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetGetConfigMsg {
    pub reply_port: u64,
}

impl NetGetConfigMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        self.reply_port.to_le_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
        })
    }
}

/// netd -> app: network config reply
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetGetConfigReplyMsg {
    pub own_ip: u32,
    pub netmask: u32,
    pub gateway: u32,
    pub dns_server: u32,
    pub mac: [u8; 6],
    pub _pad: [u8; 2],
}

impl NetGetConfigReplyMsg {
    pub const SIZE: usize = 24;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.own_ip.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.netmask.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.gateway.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.dns_server.to_le_bytes());
        bytes[16..22].copy_from_slice(&self.mac);
        bytes[22..24].copy_from_slice(&self._pad);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&bytes[16..22]);
        Some(Self {
            own_ip: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            netmask: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            gateway: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            dns_server: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
            mac,
            _pad: [bytes[22], bytes[23]],
        })
    }
}

// ============================================================================
// Date and Time Service Messages
// ============================================================================

pub const TIME_LOCALES: [&str; 6] = ["en-US", "pt-BR", "es-ES", "fr-FR", "de-DE", "ja-JP"];

pub const TIME_ZONES: [&str; 9] = [
    "UTC",
    "America/Sao_Paulo",
    "America/New_York",
    "America/Los_Angeles",
    "Europe/London",
    "Europe/Paris",
    "Asia/Tokyo",
    "Asia/Kolkata",
    "Australia/Sydney",
];

/// Client -> timesync: request the current time-service state.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TimeGetStateMsg {
    pub reply_port: u64,
}

impl TimeGetStateMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        self.reply_port.to_le_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
        })
    }
}

/// Client -> timesync: replace the user-visible date/time preferences.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TimeSetConfigMsg {
    pub reply_port: u64,
    pub automatic: bool,
    pub format_24h: bool,
    pub locale_id: u8,
    pub timezone_id: u8,
}

impl TimeSetConfigMsg {
    pub const SIZE: usize = 12;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8] = self.automatic as u8;
        bytes[9] = self.format_24h as u8;
        bytes[10] = self.locale_id;
        bytes[11] = self.timezone_id;
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let locale_id = bytes[10];
        let timezone_id = bytes[11];
        if locale_id as usize >= TIME_LOCALES.len() || timezone_id as usize >= TIME_ZONES.len() {
            return None;
        }
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            automatic: bytes[8] != 0,
            format_24h: bytes[9] != 0,
            locale_id,
            timezone_id,
        })
    }
}

/// timesync -> client: synchronized UTC epoch plus display preferences.
///
/// Clients advance `unix_seconds` from `reference_tick` using the 100 Hz
/// monotonic kernel clock. `utc_offset_minutes` is the current offset returned
/// by the internet time source, including daylight-saving time where relevant.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TimeStateReplyMsg {
    pub unix_seconds: u64,
    pub reference_tick: u64,
    pub utc_offset_minutes: i32,
    pub automatic: bool,
    pub format_24h: bool,
    pub synced: bool,
    pub syncing: bool,
    pub locale_id: u8,
    pub timezone_id: u8,
    pub last_error: u8,
}

impl TimeStateReplyMsg {
    pub const SIZE: usize = 32;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.unix_seconds.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.reference_tick.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.utc_offset_minutes.to_le_bytes());
        bytes[20] = self.automatic as u8;
        bytes[21] = self.format_24h as u8;
        bytes[22] = self.synced as u8;
        bytes[23] = self.syncing as u8;
        bytes[24] = self.locale_id;
        bytes[25] = self.timezone_id;
        bytes[26] = self.last_error;
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let locale_id = bytes[24];
        let timezone_id = bytes[25];
        if locale_id as usize >= TIME_LOCALES.len() || timezone_id as usize >= TIME_ZONES.len() {
            return None;
        }
        Some(Self {
            unix_seconds: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            reference_tick: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
            utc_offset_minutes: i32::from_le_bytes(bytes[16..20].try_into().ok()?),
            automatic: bytes[20] != 0,
            format_24h: bytes[21] != 0,
            synced: bytes[22] != 0,
            syncing: bytes[23] != 0,
            locale_id,
            timezone_id,
            last_error: bytes[26],
        })
    }

    pub fn local_unix_seconds(&self, now_tick: u64) -> i64 {
        let elapsed = now_tick.wrapping_sub(self.reference_tick) / 100;
        self.unix_seconds
            .saturating_add(elapsed)
            .saturating_add_signed(self.utc_offset_minutes as i64 * 60) as i64
    }
}

/// Client -> audiod: request current audio state.
#[derive(Debug, Clone, Copy)]
pub struct AudioGetStateMsg {
    pub reply_port: u64,
}

impl AudioGetStateMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        self.reply_port.to_le_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            reply_port: u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?),
        })
    }
}

/// Client -> audiod: update global volume and mute state.
#[derive(Debug, Clone, Copy)]
pub struct AudioSetStateMsg {
    pub reply_port: u64,
    pub volume: u8,
    pub muted: bool,
}

impl AudioSetStateMsg {
    pub const SIZE: usize = 10;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8] = self.volume.min(100);
        bytes[9] = self.muted as u8;
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            reply_port: u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?),
            volume: *bytes.get(8)?.min(&100),
            muted: *bytes.get(9)? != 0,
        })
    }
}

/// audiod -> client: current output and playback state.
#[derive(Debug, Clone, Copy)]
pub struct AudioStateReplyMsg {
    pub volume: u8,
    pub muted: bool,
    pub available: bool,
    pub playing: bool,
}

impl AudioStateReplyMsg {
    pub const SIZE: usize = 4;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        [
            self.volume.min(100),
            self.muted as u8,
            self.available as u8,
            self.playing as u8,
        ]
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            volume: (*bytes.first()?).min(100),
            muted: *bytes.get(1)? != 0,
            available: *bytes.get(2)? != 0,
            playing: *bytes.get(3)? != 0,
        })
    }
}

/// Client -> audiod: play a WAV asset. The service intentionally owns asset
/// loading, parsing, and DMA setup so applications never need hardware access.
#[derive(Debug, Clone)]
pub struct AudioPlayFileMsg {
    pub path: String,
}

impl AudioPlayFileMsg {
    pub fn to_bytes(&self) -> Vec<u8> {
        let path = self.path.as_bytes();
        let len = path.len().min(u16::MAX as usize);
        let mut bytes = Vec::with_capacity(2 + len);
        bytes.extend_from_slice(&(len as u16).to_le_bytes());
        bytes.extend_from_slice(&path[..len]);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let len = u16::from_le_bytes(bytes.get(0..2)?.try_into().ok()?) as usize;
        let path = core::str::from_utf8(bytes.get(2..2 + len)?).ok()?;
        Some(Self {
            path: String::from(path),
        })
    }
}

/// Client -> audiod: stop whatever is currently playing. Carries an optional
/// reply port so the caller can learn the resulting state (0 = no reply).
#[derive(Debug, Clone, Copy)]
pub struct AudioStopMsg {
    pub reply_port: u64,
}

impl AudioStopMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        self.reply_port.to_le_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            reply_port: u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?),
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
            app_port: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            pid: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
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
        if self.shift {
            flags |= 0x01;
        }
        if self.ctrl {
            flags |= 0x02;
        }
        if self.alt {
            flags |= 0x04;
        }
        if self.caps_lock {
            flags |= 0x08;
        }
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
    Expose = 6, // Area needs redraw
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
            address: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            width: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            height: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            stride: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            bytes_per_pixel: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            size: u64::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30],
                bytes[31],
            ]),
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
        Self {
            x,
            y,
            width,
            height,
        }
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
        if b.len() < Self::SIZE {
            return None;
        }
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
        if b.len() < Self::SIZE {
            return None;
        }
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

    pub fn ok(value: u64) -> Self {
        Self { error: 0, value }
    }
    pub fn err(code: u64) -> Self {
        Self {
            error: code,
            value: 0,
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..8].copy_from_slice(&self.error.to_le_bytes());
        b[8..16].copy_from_slice(&self.value.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            error: u64::from_le_bytes(b[0..8].try_into().ok()?),
            value: u64::from_le_bytes(b[8..16].try_into().ok()?),
        })
    }

    pub fn is_ok(&self) -> bool {
        self.error == 0
    }
}

/// On-wire stat structure (80 bytes) written into shared region by fsd.
/// Layout is fixed and shared between kernel, fsd, and userspace programs.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsStatBuf {
    pub size: u64,     // file size in bytes
    pub inode: u64,    // inode number
    pub mtime_ns: u64, // modification time (nanoseconds since epoch)
    pub atime_ns: u64, // access time
    pub ctime_ns: u64, // change time (metadata)
    pub mode: u32,     // type + permissions (POSIX-style)
    pub uid: u16,
    pub gid: u16,
    pub nlinks: u32,  // hard link count
    pub nblocks: u32, // 512-byte blocks allocated
    pub blksize: u32, // preferred I/O block size
    pub dev: u32,     // device ID (mount number)
    _reserved: [u8; 16],
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
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            size: u64::from_le_bytes(b[0..8].try_into().ok()?),
            inode: u64::from_le_bytes(b[8..16].try_into().ok()?),
            mtime_ns: u64::from_le_bytes(b[16..24].try_into().ok()?),
            atime_ns: u64::from_le_bytes(b[24..32].try_into().ok()?),
            ctime_ns: u64::from_le_bytes(b[32..40].try_into().ok()?),
            mode: u32::from_le_bytes(b[40..44].try_into().ok()?),
            uid: u16::from_le_bytes(b[44..46].try_into().ok()?),
            gid: u16::from_le_bytes(b[46..48].try_into().ok()?),
            nlinks: u32::from_le_bytes(b[48..52].try_into().ok()?),
            nblocks: u32::from_le_bytes(b[52..56].try_into().ok()?),
            blksize: u32::from_le_bytes(b[56..60].try_into().ok()?),
            dev: u32::from_le_bytes(b[60..64].try_into().ok()?),
            _reserved: [0u8; 16],
        })
    }
}

/// On-wire statvfs structure (72 bytes) for filesystem statistics.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsStatvfsBuf {
    pub bsize: u64,   // filesystem block size
    pub frsize: u64,  // fundamental block size (for f_blocks, f_bfree, f_bavail)
    pub blocks: u64,  // total data blocks
    pub bfree: u64,   // free blocks
    pub bavail: u64,  // free blocks available to non-root
    pub files: u64,   // total inodes
    pub ffree: u64,   // free inodes
    pub favail: u64,  // free inodes for non-root
    pub namemax: u32, // max filename length
    pub flags: u32,   // mount flags (ST_RDONLY etc.)
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
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            bsize: u64::from_le_bytes(b[0..8].try_into().ok()?),
            frsize: u64::from_le_bytes(b[8..16].try_into().ok()?),
            blocks: u64::from_le_bytes(b[16..24].try_into().ok()?),
            bfree: u64::from_le_bytes(b[24..32].try_into().ok()?),
            bavail: u64::from_le_bytes(b[32..40].try_into().ok()?),
            files: u64::from_le_bytes(b[40..48].try_into().ok()?),
            ffree: u64::from_le_bytes(b[48..56].try_into().ok()?),
            favail: u64::from_le_bytes(b[56..64].try_into().ok()?),
            namemax: u32::from_le_bytes(b[64..68].try_into().ok()?),
            flags: u32::from_le_bytes(b[68..72].try_into().ok()?),
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
    pos: usize,
}

impl<'a> FsDirentIter<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

#[derive(Debug, Clone)]
pub struct FsDirentEntry {
    pub ino: u32,
    pub file_type: u8,
    pub name: alloc::string::String,
}

impl<'a> Iterator for FsDirentIter<'a> {
    type Item = FsDirentEntry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 8 > self.data.len() {
            return None;
        }
        let b = &self.data[self.pos..];
        let ino = u32::from_le_bytes(b[0..4].try_into().ok()?);
        let rec_len = u16::from_le_bytes(b[4..6].try_into().ok()?) as usize;
        let name_len = b[6] as usize;
        let file_type = b[7];

        if rec_len < 8 || !rec_len.is_multiple_of(4) || self.pos + rec_len > self.data.len() {
            return None;
        }
        if 8 + name_len > rec_len {
            return None;
        }

        let name = String::from(core::str::from_utf8(&b[8..8 + name_len]).unwrap_or(""));

        self.pos += rec_len;
        if ino == 0 {
            return self.next();
        } // skip deleted entries

        Some(FsDirentEntry {
            ino,
            file_type,
            name,
        })
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
        let port = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
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
        let reply_port = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
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
            port: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
        })
    }
}

/// Message to unregister a service from the name service.
///
/// Authorisation is performed by namesvc using the kernel IPC envelope (the
/// sender must be the current owner of the registration), never the payload.
#[derive(Debug, Clone, Copy)]
pub struct NsUnregisterMsg {
    /// The name of the service to unregister (fixed-size, null padded).
    pub name: [u8; 32],
}

impl NsUnregisterMsg {
    pub const SIZE: usize = 32;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        self.name
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut name = [0u8; 32];
        name.copy_from_slice(&bytes[0..32]);
        Some(Self { name })
    }
}

// ============================================================================
// App Launcher Protocol Messages (1200-1299)
// ============================================================================

/// Status codes returned in AppLaunchReplyMsg.status.
pub mod launch_status {
    /// Launch succeeded; `pid` field is valid.
    pub const LAUNCH_OK: u32 = 0;
    /// The .atxf file was not found on the filesystem.
    pub const LAUNCH_ERR_NOTFOUND: u32 = 1;
    /// The file exists but is not a valid ATXF image (bad magic, truncated,
    /// unsupported version, etc.).
    pub const LAUNCH_ERR_INVALID: u32 = 2;
    /// Not enough physical memory to map the new process.
    pub const LAUNCH_ERR_NOMEM: u32 = 3;
    /// Path is empty, exceeds maximum length, is not absolute, or contains `..`.
    pub const LAUNCH_ERR_BADPATH: u32 = 4;
    /// Path does not end in ".atxf" — wrong file type.
    pub const LAUNCH_ERR_BADTYPE: u32 = 5;
    /// Filesystem service is unavailable (kernel FAT32 not initialised).
    pub const LAUNCH_ERR_NOFS: u32 = 6;
    /// Unspecified internal error in the app_launcher.
    pub const LAUNCH_ERR_INTERNAL: u32 = 99;
}

/// Maximum path length accepted in an AppLaunchRequestMsg.
pub const APP_LAUNCH_MAX_PATH: usize = 248;

/// Current protocol version for the app-launcher IPC.
pub const APP_LAUNCH_VERSION: u32 = 1;

/// Request sent by a client (e.g. file manager) to the `app_launcher` service.
///
/// Layout (little-endian, total 264 bytes):
///   bytes  0..8  : reply_port (u64)
///   bytes  8..12 : protocol_version (u32) — must be `APP_LAUNCH_VERSION`
///   bytes 12..16 : path_len (u32)         — byte length of the path
///   bytes 16..264: path ([u8; 248])        — UTF-8 absolute path, zero-padded
#[derive(Clone, Copy)]
pub struct AppLaunchRequestMsg {
    /// Port the launcher should send `AppLaunchReply` back to.
    pub reply_port: u64,
    /// Protocol version — set to `APP_LAUNCH_VERSION` (currently 1).
    pub protocol_version: u32,
    /// Byte length of the meaningful prefix of `path`.
    pub path_len: u32,
    /// Filesystem path of the ATXF binary to execute, zero-padded.
    pub path: [u8; APP_LAUNCH_MAX_PATH],
}

impl AppLaunchRequestMsg {
    pub const SIZE: usize = 8 + 4 + 4 + APP_LAUNCH_MAX_PATH; // 264

    /// Construct a new request.  Returns `None` if `path` exceeds `APP_LAUNCH_MAX_PATH`.
    pub fn new(reply_port: u64, path: &str) -> Option<Self> {
        let path_bytes = path.as_bytes();
        if path_bytes.len() > APP_LAUNCH_MAX_PATH {
            return None;
        }
        let mut msg = Self {
            reply_port,
            protocol_version: APP_LAUNCH_VERSION,
            path_len: path_bytes.len() as u32,
            path: [0u8; APP_LAUNCH_MAX_PATH],
        };
        msg.path[..path_bytes.len()].copy_from_slice(path_bytes);
        Some(msg)
    }

    /// Return the path as a `&str` (without trailing zero bytes).
    pub fn path_str(&self) -> Option<&str> {
        let len = self.path_len as usize;
        if len > APP_LAUNCH_MAX_PATH {
            return None;
        }
        core::str::from_utf8(&self.path[..len]).ok()
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        buf[8..12].copy_from_slice(&self.protocol_version.to_le_bytes());
        buf[12..16].copy_from_slice(&self.path_len.to_le_bytes());
        buf[16..16 + APP_LAUNCH_MAX_PATH].copy_from_slice(&self.path);
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let reply_port = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
        let protocol_version = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        let path_len = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
        let mut path = [0u8; APP_LAUNCH_MAX_PATH];
        path.copy_from_slice(&bytes[16..16 + APP_LAUNCH_MAX_PATH]);
        Some(Self {
            reply_port,
            protocol_version,
            path_len,
            path,
        })
    }
}

/// Maximum byte length of the human-readable error message in AppLaunchReplyMsg.
pub const APP_LAUNCH_ERR_MSG_MAX: usize = 60;

/// Reply sent by the `app_launcher` service back to the requesting process.
///
/// Layout (little-endian, total 76 bytes):
///   bytes  0..4  : status (u32)          — 0 = success; see `launch_status`
///   bytes  4..12 : pid (u64)             — PID of new process (0 on error)
///   bytes 12..16 : err_msg_len (u32)     — byte length of error message
///   bytes 16..76 : err_msg ([u8; 60])    — human-readable error, zero-padded
#[derive(Clone, Copy)]
pub struct AppLaunchReplyMsg {
    /// Launch status.  `0` = success.  See `launch_status` constants.
    pub status: u32,
    /// PID of the spawned process.  Valid only when `status == LAUNCH_OK`.
    pub pid: u64,
    /// Byte length of the human-readable error string.
    pub err_msg_len: u32,
    /// Human-readable error message (UTF-8, zero-padded); all-zeros on success.
    pub err_msg: [u8; APP_LAUNCH_ERR_MSG_MAX],
}

impl AppLaunchReplyMsg {
    pub const SIZE: usize = 4 + 8 + 4 + APP_LAUNCH_ERR_MSG_MAX; // 76

    /// Construct a success reply carrying the new process ID.
    pub fn success(pid: u64) -> Self {
        Self {
            status: launch_status::LAUNCH_OK,
            pid,
            err_msg_len: 0,
            err_msg: [0u8; APP_LAUNCH_ERR_MSG_MAX],
        }
    }

    /// Construct an error reply with a human-readable message.
    pub fn error(status: u32, msg: &str) -> Self {
        let msg_bytes = msg.as_bytes();
        let len = msg_bytes.len().min(APP_LAUNCH_ERR_MSG_MAX);
        let mut err_msg = [0u8; APP_LAUNCH_ERR_MSG_MAX];
        err_msg[..len].copy_from_slice(&msg_bytes[..len]);
        Self {
            status,
            pid: 0,
            err_msg_len: len as u32,
            err_msg,
        }
    }

    /// Return the error message as a `&str` (empty on success or if not valid UTF-8).
    pub fn err_msg_str(&self) -> &str {
        let len = (self.err_msg_len as usize).min(APP_LAUNCH_ERR_MSG_MAX);
        core::str::from_utf8(&self.err_msg[..len]).unwrap_or("")
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&self.status.to_le_bytes());
        buf[4..12].copy_from_slice(&self.pid.to_le_bytes());
        buf[12..16].copy_from_slice(&self.err_msg_len.to_le_bytes());
        buf[16..16 + APP_LAUNCH_ERR_MSG_MAX].copy_from_slice(&self.err_msg);
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let status = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
        let pid = u64::from_le_bytes(bytes[4..12].try_into().ok()?);
        let err_msg_len = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
        let mut err_msg = [0u8; APP_LAUNCH_ERR_MSG_MAX];
        err_msg.copy_from_slice(&bytes[16..16 + APP_LAUNCH_ERR_MSG_MAX]);
        Some(Self {
            status,
            pid,
            err_msg_len,
            err_msg,
        })
    }
}
// ============================================================================
// Display Settings Protocol (1300-1399)
// ============================================================================

/// Wallpaper source type for Display Settings
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WallpaperSourceType {
    Image = 0,
    SolidColor = 1,
}

impl WallpaperSourceType {
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Image),
            1 => Some(Self::SolidColor),
            _ => None,
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            Self::Image => "Image",
            Self::SolidColor => "SolidColor",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Image" => Some(Self::Image),
            "SolidColor" => Some(Self::SolidColor),
            _ => None,
        }
    }
}

/// Scaling mode for wallpaper images
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScalingMode {
    Fill = 0,
    Fit = 1,
    Stretch = 2,
    Center = 3,
    Tile = 4,
}

impl ScalingMode {
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Fill),
            1 => Some(Self::Fit),
            2 => Some(Self::Stretch),
            3 => Some(Self::Center),
            4 => Some(Self::Tile),
            _ => None,
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            Self::Fill => "Fill",
            Self::Fit => "Fit",
            Self::Stretch => "Stretch",
            Self::Center => "Center",
            Self::Tile => "Tile",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Fill" => Some(Self::Fill),
            "Fit" => Some(Self::Fit),
            "Stretch" => Some(Self::Stretch),
            "Center" => Some(Self::Center),
            "Tile" => Some(Self::Tile),
            _ => None,
        }
    }
}

// ============================================================================
// Display Settings Protocol Messages (1300-1399)
// ============================================================================

/// Request to open Display Settings in a specific tab
#[derive(Debug, Clone)]
pub struct OpenInTabMsg {
    pub target_app: String,
    pub tab_name: String,
}

impl OpenInTabMsg {
    pub const MAX_TARGET_APP_LEN: usize = 64;
    pub const MAX_TAB_NAME_LEN: usize = 32;

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let target_bytes = self.target_app.as_bytes();
        let tab_bytes = self.tab_name.as_bytes();

        bytes.extend_from_slice(&(target_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(target_bytes);
        bytes.extend_from_slice(&(tab_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(tab_bytes);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }

        let target_len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        if target_len == 0 || target_len > Self::MAX_TARGET_APP_LEN {
            return None;
        }
        if bytes.len() < 4 + target_len + 4 {
            return None;
        }

        let target_app = core::str::from_utf8(&bytes[4..4 + target_len]).ok()?.trim();
        if target_app.is_empty() {
            return None;
        }

        let tab_len_offset = 4 + target_len;
        let tab_len = u32::from_le_bytes([
            bytes[tab_len_offset],
            bytes[tab_len_offset + 1],
            bytes[tab_len_offset + 2],
            bytes[tab_len_offset + 3],
        ]) as usize;
        if tab_len == 0 || tab_len > Self::MAX_TAB_NAME_LEN {
            return None;
        }
        if bytes.len() < tab_len_offset + 4 + tab_len {
            return None;
        }

        if bytes.len() != tab_len_offset + 4 + tab_len {
            return None;
        }

        let tab_name =
            core::str::from_utf8(&bytes[tab_len_offset + 4..tab_len_offset + 4 + tab_len])
                .ok()?
                .trim();
        if tab_name != "Wallpaper"
            && tab_name != "Resolution"
            && tab_name != "Date and Time"
            && tab_name != "DateTime"
        {
            return None;
        }

        Some(Self {
            target_app: String::from(target_app),
            tab_name: String::from(tab_name),
        })
    }
}

/// Request to apply wallpaper configuration
#[derive(Debug, Clone)]
pub struct ApplyWallpaperMsg {
    pub source_type: WallpaperSourceType,
    pub image_path: Option<String>,
    pub color_rgb: Option<u32>,
    pub scaling_mode: ScalingMode,
}

impl ApplyWallpaperMsg {
    pub const MAX_PATH_LEN: usize = 256;

    fn validate_image_path(path: &str) -> bool {
        if path.is_empty() || path.len() > Self::MAX_PATH_LEN {
            return false;
        }
        if !path.starts_with("/system/wallpapers/") || path.contains("..") {
            return false;
        }
        let lower = path.to_ascii_lowercase();
        lower.ends_with(".jpg") || lower.ends_with(".jpeg")
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(self.source_type.to_u8());
        bytes.push(self.scaling_mode.to_u8());

        match self.source_type {
            WallpaperSourceType::Image => {
                let path = self
                    .image_path
                    .as_ref()
                    .expect("Image source requires path");
                let path_bytes = path.as_bytes();
                bytes.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
                bytes.extend_from_slice(path_bytes);
            }
            WallpaperSourceType::SolidColor => {
                let rgb = self.color_rgb.expect("SolidColor source requires RGB");
                bytes.extend_from_slice(&rgb.to_le_bytes());
            }
        }
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 2 {
            return None;
        }

        let source_type = WallpaperSourceType::from_u8(bytes[0])?;
        let scaling_mode = ScalingMode::from_u8(bytes[1])?;

        let (image_path, color_rgb) = match source_type {
            WallpaperSourceType::Image => {
                if bytes.len() < 6 {
                    return None;
                }
                let path_len =
                    u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;
                if path_len > Self::MAX_PATH_LEN {
                    return None;
                }
                if bytes.len() < 6 + path_len {
                    return None;
                }
                let path = core::str::from_utf8(&bytes[6..6 + path_len]).ok()?;
                if !Self::validate_image_path(path) {
                    return None;
                }
                if bytes.len() != 6 + path_len {
                    return None;
                }
                (Some(String::from(path)), None)
            }
            WallpaperSourceType::SolidColor => {
                if bytes.len() < 6 {
                    return None;
                }
                if bytes.len() != 6 {
                    return None;
                }
                let rgb = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                if rgb > 0x00FF_FFFF {
                    return None;
                }
                (None, Some(rgb))
            }
        };

        Some(Self {
            source_type,
            image_path,
            color_rgb,
            scaling_mode,
        })
    }
}

/// Notification that wallpaper was applied successfully
#[derive(Debug, Clone, Copy)]
pub struct WallpaperAppliedMsg {
    // Empty payload - just acknowledgment
}

impl WallpaperAppliedMsg {
    pub const SIZE: usize = 0;

    pub fn to_bytes(&self) -> [u8; 0] {
        []
    }

    pub fn from_bytes(_bytes: &[u8]) -> Option<Self> {
        Some(Self {})
    }
}

/// Notification that wallpaper application failed
#[derive(Debug, Clone)]
pub struct WallpaperFailedMsg {
    pub error_message: String,
}

impl WallpaperFailedMsg {
    pub const MAX_ERROR_LEN: usize = 128;

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let msg_bytes = self.error_message.as_bytes();
        let len = msg_bytes.len().min(Self::MAX_ERROR_LEN);
        bytes.extend_from_slice(&(len as u32).to_le_bytes());
        bytes.extend_from_slice(&msg_bytes[..len]);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        if len == 0 || len > Self::MAX_ERROR_LEN {
            return None;
        }
        if bytes.len() < 4 + len {
            return None;
        }
        if bytes.len() != 4 + len {
            return None;
        }
        let msg = core::str::from_utf8(&bytes[4..4 + len]).ok()?;
        Some(Self {
            error_message: String::from(msg),
        })
    }
}

// ============================================================================
// Networking Messages (1400-1499)
// ============================================================================

/// Network IP Address (IPv4 or IPv6)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct NetIpAddr {
    pub family: u8, // 4 = IPv4, 6 = IPv6
    pub data: [u8; 16],
}

impl NetIpAddr {
    pub fn ipv4(addr: [u8; 4]) -> Self {
        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(&addr);
        Self { family: 4, data }
    }

    pub fn ipv6(addr: [u8; 16]) -> Self {
        Self {
            family: 6,
            data: addr,
        }
    }

    pub fn to_bytes(&self) -> [u8; 17] {
        let mut bytes = [0u8; 17];
        bytes[0] = self.family;
        bytes[1..17].copy_from_slice(&self.data);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 17 {
            return None;
        }
        let mut data = [0u8; 16];
        data.copy_from_slice(&bytes[1..17]);
        Some(Self {
            family: bytes[0],
            data,
        })
    }
}

/// netd -> nic_driver: Provide shared memory region for Ring Buffers
#[derive(Debug, Clone, Copy)]
pub struct NetAssignRingsMsg {
    pub region_id: u64,
    pub ring_capacity: u32,
}

impl NetAssignRingsMsg {
    pub const SIZE: usize = 12;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.region_id.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.ring_capacity.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            region_id: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            ring_capacity: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
        })
    }
}

/// app -> netd: ICMP Echo Request
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetIcmpEchoRequestMsg {
    pub reply_port: u64,
    pub dest_ip: NetIpAddr,
    pub sequence: u16,
    pub timeout_ms: u32,
    pub payload_len: u32,
    pub payload: [u8; 64],
}

impl NetIcmpEchoRequestMsg {
    pub const SIZE: usize = 8 + 17 + 2 + 4 + 4 + 64;

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SIZE);
        bytes.extend_from_slice(&self.reply_port.to_le_bytes());
        bytes.extend_from_slice(&self.dest_ip.to_bytes());
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.timeout_ms.to_le_bytes());
        bytes.extend_from_slice(&self.payload_len.to_le_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let reply_port = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
        let dest_ip = NetIpAddr::from_bytes(&bytes[8..25])?;
        let sequence = u16::from_le_bytes(bytes[25..27].try_into().ok()?);
        let timeout_ms = u32::from_le_bytes(bytes[27..31].try_into().ok()?);
        let payload_len = u32::from_le_bytes(bytes[31..35].try_into().ok()?);
        let mut payload = [0u8; 64];
        payload.copy_from_slice(&bytes[35..99]);
        Some(Self {
            reply_port,
            dest_ip,
            sequence,
            timeout_ms,
            payload_len,
            payload,
        })
    }
}

/// netd -> app: ICMP Echo Reply
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetIcmpEchoReplyMsg {
    pub src_ip: NetIpAddr,
    pub sequence: u16,
    pub ttl: u8,
    pub rtt_ms: u32,
    pub error: u32, // 0 = success, 1 = timeout, 2 = other
    pub payload_len: u32,
    pub payload: [u8; 64],
}

impl NetIcmpEchoReplyMsg {
    pub const SIZE: usize = 17 + 2 + 1 + 4 + 4 + 4 + 64;

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SIZE);
        bytes.extend_from_slice(&self.src_ip.to_bytes());
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.push(self.ttl);
        bytes.extend_from_slice(&self.rtt_ms.to_le_bytes());
        bytes.extend_from_slice(&self.error.to_le_bytes());
        bytes.extend_from_slice(&self.payload_len.to_le_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let src_ip = NetIpAddr::from_bytes(&bytes[0..17])?;
        let sequence = u16::from_le_bytes(bytes[17..19].try_into().ok()?);
        let ttl = bytes[19];
        let rtt_ms = u32::from_le_bytes(bytes[20..24].try_into().ok()?);
        let error = u32::from_le_bytes(bytes[24..28].try_into().ok()?);
        let payload_len = u32::from_le_bytes(bytes[28..32].try_into().ok()?);
        let mut payload = [0u8; 64];
        payload.copy_from_slice(&bytes[32..96]);
        Some(Self {
            src_ip,
            sequence,
            ttl,
            rtt_ms,
            error,
            payload_len,
            payload,
        })
    }
}

/// nic_driver -> netd: driver is ready, includes MAC address
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetDriverReadyMsg {
    pub mac: [u8; 6],
    pub _pad: [u8; 2],
}

impl NetDriverReadyMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..6].copy_from_slice(&self.mac);
        bytes[6..8].copy_from_slice(&self._pad);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&bytes[0..6]);
        Some(Self {
            mac,
            _pad: [bytes[6], bytes[7]],
        })
    }
}

/// init -> netd: configure IP/mask/gateway/DNS
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetConfigureMsg {
    pub own_ip: u32,
    pub netmask: u32,
    pub gateway: u32,
    pub dns_server: u32,
}

impl NetConfigureMsg {
    pub const SIZE: usize = 16;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.own_ip.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.netmask.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.gateway.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.dns_server.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            own_ip: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            netmask: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            gateway: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            dns_server: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
        })
    }
}

/// app -> netd: create a socket
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetSocketMsg {
    pub reply_port: u64,
    pub proto: u8, // 0=TCP, 1=UDP
    pub _pad: [u8; 7],
}

impl NetSocketMsg {
    pub const SIZE: usize = 16;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8] = self.proto;
        bytes[9..16].copy_from_slice(&self._pad);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut pad = [0u8; 7];
        pad.copy_from_slice(&bytes[9..16]);
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            proto: bytes[8],
            _pad: pad,
        })
    }
}

/// netd -> app: socket creation reply
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetSocketReplyMsg {
    pub socket_id: u32,
    pub error: u32,
}

impl NetSocketReplyMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.error.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            socket_id: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            error: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
        })
    }
}

/// app -> netd: connect TCP socket to remote
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetConnectMsg {
    pub reply_port: u64,
    pub socket_id: u32,
    pub remote_ip: u32,
    pub remote_port: u16,
    pub _pad: [u8; 2],
}

impl NetConnectMsg {
    pub const SIZE: usize = 20;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.remote_ip.to_le_bytes());
        bytes[16..18].copy_from_slice(&self.remote_port.to_le_bytes());
        bytes[18..20].copy_from_slice(&self._pad);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            socket_id: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            remote_ip: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
            remote_port: u16::from_le_bytes(bytes[16..18].try_into().ok()?),
            _pad: [bytes[18], bytes[19]],
        })
    }
}

/// netd -> app: connect reply
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetConnectReplyMsg {
    pub socket_id: u32,
    pub error: u32,
}

impl NetConnectReplyMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.error.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            socket_id: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            error: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
        })
    }
}

/// app -> netd: send data over socket
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetSendMsg {
    pub reply_port: u64,
    pub socket_id: u32,
    pub len: u32,
    pub data: [u8; 1024],
}

impl NetSendMsg {
    pub const SIZE: usize = 1040;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.len.to_le_bytes());
        bytes[16..1040].copy_from_slice(&self.data);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut data = [0u8; 1024];
        data.copy_from_slice(&bytes[16..1040]);
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            socket_id: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            len: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
            data,
        })
    }
}

/// netd -> app: send reply
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetSendReplyMsg {
    pub socket_id: u32,
    pub sent: u32,
    pub error: u32,
}

impl NetSendReplyMsg {
    pub const SIZE: usize = 12;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.sent.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.error.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            socket_id: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            sent: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            error: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
        })
    }
}

/// app -> netd: receive data from socket
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetRecvMsg {
    pub reply_port: u64,
    pub socket_id: u32,
    pub max_len: u32,
    pub timeout_ms: u32,
}

impl NetRecvMsg {
    pub const SIZE: usize = 20;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.max_len.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.timeout_ms.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            socket_id: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            max_len: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
            timeout_ms: u32::from_le_bytes(bytes[16..20].try_into().ok()?),
        })
    }
}

/// netd -> app: receive reply with data
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetRecvReplyMsg {
    pub socket_id: u32,
    pub len: u32,
    pub error: u32,
    pub data: [u8; 1024],
}

impl NetRecvReplyMsg {
    pub const SIZE: usize = 1036;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.len.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.error.to_le_bytes());
        bytes[12..1036].copy_from_slice(&self.data);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut data = [0u8; 1024];
        data.copy_from_slice(&bytes[12..1036]);
        Some(Self {
            socket_id: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            len: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            error: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            data,
        })
    }
}

/// app -> netd: close socket
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetCloseMsg {
    pub reply_port: u64,
    pub socket_id: u32,
    pub _pad: [u8; 4],
}

impl NetCloseMsg {
    pub const SIZE: usize = 16;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[12..16].copy_from_slice(&self._pad);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut pad = [0u8; 4];
        pad.copy_from_slice(&bytes[12..16]);
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            socket_id: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            _pad: pad,
        })
    }
}

/// netd -> app: close reply
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetCloseReplyMsg {
    pub socket_id: u32,
    pub error: u32,
}

impl NetCloseReplyMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.socket_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.error.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            socket_id: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            error: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
        })
    }
}

/// app -> netd: resolve hostname to IP
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetResolveMsg {
    pub reply_port: u64,
    pub name_len: u32,
    pub name: [u8; 256],
}

impl NetResolveMsg {
    pub const SIZE: usize = 268;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.reply_port.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.name_len.to_le_bytes());
        bytes[12..268].copy_from_slice(&self.name);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut name = [0u8; 256];
        name.copy_from_slice(&bytes[12..268]);
        Some(Self {
            reply_port: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            name_len: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            name,
        })
    }
}

/// netd -> app: resolve reply
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetResolveReplyMsg {
    pub ip: u32,
    pub error: u32,
}

impl NetResolveReplyMsg {
    pub const SIZE: usize = 8;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.ip.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.error.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            ip: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            error: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_tab_roundtrip() {
        let msg = OpenInTabMsg {
            target_app: String::from("display_settings"),
            tab_name: String::from("Wallpaper"),
        };

        let decoded = OpenInTabMsg::from_bytes(&msg.to_bytes()).unwrap();
        assert_eq!(decoded.target_app, "display_settings");
        assert_eq!(decoded.tab_name, "Wallpaper");
    }

    #[test]
    fn open_in_tab_rejects_invalid_tab_name() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(16u32).to_le_bytes());
        bytes.extend_from_slice(b"display_settings");
        bytes.extend_from_slice(&(7u32).to_le_bytes());
        bytes.extend_from_slice(b"Invalid");
        assert!(OpenInTabMsg::from_bytes(&bytes).is_none());
    }

    #[test]
    fn apply_wallpaper_roundtrip_image() {
        let msg = ApplyWallpaperMsg {
            source_type: WallpaperSourceType::Image,
            image_path: Some(String::from("/system/wallpapers/mountain.jpg")),
            color_rgb: None,
            scaling_mode: ScalingMode::Fit,
        };

        let decoded = ApplyWallpaperMsg::from_bytes(&msg.to_bytes()).unwrap();
        assert_eq!(decoded.source_type, WallpaperSourceType::Image);
        assert_eq!(
            decoded.image_path.as_deref(),
            Some("/system/wallpapers/mountain.jpg")
        );
        assert_eq!(decoded.scaling_mode, ScalingMode::Fit);
    }

    #[test]
    fn apply_wallpaper_rejects_path_traversal() {
        let mut bytes = Vec::new();
        bytes.push(WallpaperSourceType::Image.to_u8());
        bytes.push(ScalingMode::Fill.to_u8());
        let path = "/system/wallpapers/../secret.jpg";
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
        assert!(ApplyWallpaperMsg::from_bytes(&bytes).is_none());
    }

    #[test]
    fn apply_wallpaper_rejects_out_of_range_rgb() {
        let mut bytes = Vec::new();
        bytes.push(WallpaperSourceType::SolidColor.to_u8());
        bytes.push(ScalingMode::Fill.to_u8());
        bytes.extend_from_slice(&0x01FF_FFFFu32.to_le_bytes());
        assert!(ApplyWallpaperMsg::from_bytes(&bytes).is_none());
    }

    #[test]
    fn wallpaper_failed_rejects_empty_message() {
        let bytes = 0u32.to_le_bytes();
        assert!(WallpaperFailedMsg::from_bytes(&bytes).is_none());
    }

    #[test]
    fn time_state_roundtrip() {
        let state = TimeStateReplyMsg {
            unix_seconds: 1_765_000_000,
            reference_tick: 12_345,
            utc_offset_minutes: -180,
            automatic: true,
            format_24h: true,
            synced: true,
            syncing: false,
            locale_id: 1,
            timezone_id: 1,
            last_error: 0,
        };
        let decoded = TimeStateReplyMsg::from_bytes(&state.to_bytes()).unwrap();
        assert_eq!(decoded.unix_seconds, state.unix_seconds);
        assert_eq!(decoded.utc_offset_minutes, -180);
        assert!(decoded.synced);
        assert_eq!(decoded.locale_id, 1);
        assert_eq!(decoded.timezone_id, 1);
    }

    #[test]
    fn time_config_rejects_unknown_timezone() {
        let mut bytes = TimeSetConfigMsg {
            reply_port: 7,
            automatic: true,
            format_24h: false,
            locale_id: 0,
            timezone_id: 0,
        }
        .to_bytes();
        bytes[11] = TIME_ZONES.len() as u8;
        assert!(TimeSetConfigMsg::from_bytes(&bytes).is_none());
    }

    #[test]
    fn audio_messages_roundtrip() {
        let set = AudioSetStateMsg {
            reply_port: 42,
            volume: 85,
            muted: true,
        };
        let decoded_set = AudioSetStateMsg::from_bytes(&set.to_bytes()).unwrap();
        assert_eq!(decoded_set.reply_port, 42);
        assert_eq!(decoded_set.volume, 85);
        assert!(decoded_set.muted);

        let play = AudioPlayFileMsg {
            path: String::from("/system/sounds/startup.wav"),
        };
        let decoded_play = AudioPlayFileMsg::from_bytes(&play.to_bytes()).unwrap();
        assert_eq!(decoded_play.path, play.path);

        let stop = AudioStopMsg { reply_port: 7 };
        let decoded_stop = AudioStopMsg::from_bytes(&stop.to_bytes()).unwrap();
        assert_eq!(decoded_stop.reply_port, 7);
        assert_eq!(MessageType::from_u32(1604), Some(MessageType::AudioStop));
    }

    #[test]
    fn audio_play_rejects_truncated_path() {
        assert!(AudioPlayFileMsg::from_bytes(&[8, 0, b's']).is_none());
    }
}
