// Bochs Graphics Adapter (BGA) / VBE_DISPI Driver
//
// Implements the Bochs Graphics Adapter, also known as the VBE DISPI interface.
// This is the standard virtual graphics device exposed by QEMU when using the
// "qemu64" machine type with the Bochs VGA device (vendor 0x1234, device 0x1111).
//
// ## Hardware Architecture
//
// The BGA exposes two I/O ports for register access:
//   - 0x01CE: Index register (select which VBE DISPI register to access)
//   - 0x01CF: Data register  (read or write the selected register value)
//
// The Linear Framebuffer (LFB) physical address is obtained from PCI BAR0.
// Unlike the legacy banked VGA framebuffer at 0xA0000, the LFB can be at any
// physical address (typically 0xE0000000 in QEMU but MUST NOT be hardcoded).
//
// ## VBE DISPI Register Map
//
//   Index 0 (ID):          Version identifier (must be >= 0xB0C5)
//   Index 1 (XRES):        Horizontal resolution in pixels (aligned to 8)
//   Index 2 (YRES):        Vertical resolution in pixels
//   Index 3 (BPP):         Bits per pixel (4, 8, 15, 16, 24, 32)
//   Index 4 (ENABLE):      Enable/disable VBE mode (see flags below)
//   Index 5 (BANK):        Video memory bank (used in banked mode, irrelevant for LFB)
//   Index 6 (VIRT_WIDTH):  Virtual framebuffer width (for scrolling)
//   Index 7 (VIRT_HEIGHT): Virtual framebuffer height
//   Index 8 (X_OFFSET):    X scrolling offset within virtual framebuffer
//   Index 9 (Y_OFFSET):    Y scrolling offset within virtual framebuffer
//
// ## ENABLE Register Flags
//
//   Bit 0 (0x01): VBE_DISPI_ENABLED        — Enable VBE mode
//   Bit 1 (0x02): VBE_DISPI_GETCAPS        — Read capability values (used when reading XRES/YRES)
//   Bit 5 (0x20): VBE_DISPI_8BIT_DAC       — Use 8-bit DAC (for palette modes)
//   Bit 6 (0x40): VBE_DISPI_LFB_ENABLED    — Use linear framebuffer instead of banked
//   Bit 7 (0x80): VBE_DISPI_NOCLEARMEM     — Do not clear video memory on mode set
//
// ## Mode-Setting Procedure
//
//   1. Write 0 to ENABLE register (disable VBE)
//   2. Write target XRES
//   3. Write target YRES
//   4. Write target BPP
//   5. Write ENABLED | LFB_ENABLED (and optionally NOCLEARMEM) to ENABLE
//
// ## PCI Configuration Space Access
//
//   Configuration space is accessed via port I/O at:
//     - 0xCF8: PCI_CONFIG_ADDRESS (write a 32-bit address specifying bus/slot/func/offset)
//     - 0xCFC: PCI_CONFIG_DATA    (read/write the 32-bit register at the selected address)
//
//   BAR0 is at config space offset 0x10. For memory BARs, bits [3:0] contain type flags.
//   Bit 0 = 0 means memory bar. The actual address is bits [31:4] for 32-bit BARs,
//   or bits [63:4] spanning BAR0+BAR1 for 64-bit BARs.
//
// ## Double Buffering
//
//   This driver manages a kernel-side state record of the current video mode,
//   but does NOT maintain a double-buffer itself (that would waste kernel memory).
//   Instead, the LFB address and pitch are published via kernel_video_info(), and
//   the display userspace driver allocates its own backbuffer and blits it to the LFB.
//
//   Rationale: Kernel memory is precious. The userspace display driver knows its
//   own rendering requirements and can allocate an appropriately sized backbuffer.
//
// ## Safety Model
//
//   All register accesses use raw port I/O (unsafe). This code runs only in
//   ring 0 (kernel mode). Hardware state is protected by the BGA_STATE spinlock,
//   which must be acquired before any register modification sequence.
//
//   The mode-switch sequence is NOT atomic from the hardware perspective; between
//   DISABLE and RE-ENABLE, the screen will briefly show garbage. This is acceptable
//   for a mode switch but means mode switches should not be triggered under display-
//   critical conditions.
//
// ## Limitations and Future Work
//
//   - Currently only supports 32 BPP (BGRA format)
//   - Virtual framebuffer scrolling is not implemented (VIRT_WIDTH = XRES)
//   - 24-bit BPP is excluded because its non-power-of-2 stride complicates blitting
//   - EDID is not queried (would require DDC/CI over I2C, which BGA does not support)
//   - SMP: The spinlock ensures mutual exclusion but TLB shootdowns after remapping
//     the framebuffer must be added before multi-core support is complete

#![allow(dead_code)]

use spin::Mutex;
use crate::{log_info, log_warn, log_error};
use crate::mm::vm;

// ============================================================================
// Error Type
// ============================================================================

/// Typed errors returned by BGA mode-setting operations.
///
/// Using a typed enum rather than `&'static str` lets callers match on
/// specific failure modes without brittle substring checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgaError {
    /// Width or height is zero.
    InvalidResolution,
    /// Width is not a multiple of 8 (BGA hardware requirement).
    WidthMisaligned,
    /// Only 32 BPP is supported by this driver.
    UnsupportedBpp,
    /// The requested mode would exceed the mapped LFB region.
    ExceedsLfbSize,
    /// BGA hardware is not present or not initialised.
    NotAvailable,
    /// Hardware did not accept the programmed resolution on readback.
    HardwareRejected,
}

// ============================================================================
// VBE DISPI I/O Port Definitions
// ============================================================================

/// Index register: write the register index here before accessing the data register.
const VBE_DISPI_PORT_INDEX: u16 = 0x01CE;

/// Data register: read/write register value after setting the index.
const VBE_DISPI_PORT_DATA: u16 = 0x01CF;

// VBE DISPI register indices
const VBE_DISPI_INDEX_ID:          u16 = 0x00;
const VBE_DISPI_INDEX_XRES:        u16 = 0x01;
const VBE_DISPI_INDEX_YRES:        u16 = 0x02;
const VBE_DISPI_INDEX_BPP:         u16 = 0x03;
const VBE_DISPI_INDEX_ENABLE:      u16 = 0x04;
const VBE_DISPI_INDEX_BANK:        u16 = 0x05;
const VBE_DISPI_INDEX_VIRT_WIDTH:  u16 = 0x06;
const VBE_DISPI_INDEX_VIRT_HEIGHT: u16 = 0x07;
const VBE_DISPI_INDEX_X_OFFSET:    u16 = 0x08;
const VBE_DISPI_INDEX_Y_OFFSET:    u16 = 0x09;

// VBE DISPI supported version IDs.
// 0xB0C0 is the first version. 0xB0C5 introduced GETCAPS and virtual framebuffer.
const VBE_DISPI_ID0: u16 = 0xB0C0;
const VBE_DISPI_ID1: u16 = 0xB0C1;
const VBE_DISPI_ID2: u16 = 0xB0C2;
const VBE_DISPI_ID3: u16 = 0xB0C3;
const VBE_DISPI_ID4: u16 = 0xB0C4;
const VBE_DISPI_ID5: u16 = 0xB0C5; // Minimum required version

// ENABLE register flag bits
const VBE_DISPI_DISABLED:  u16 = 0x00;
const VBE_DISPI_ENABLED:   u16 = 0x01;
const VBE_DISPI_GETCAPS:   u16 = 0x02; // When set, read XRES/YRES returns maximums
const VBE_DISPI_8BIT_DAC:  u16 = 0x20;
const VBE_DISPI_LFB_ENABLED:  u16 = 0x40;
const VBE_DISPI_NOCLEARMEM:   u16 = 0x80;

// Supported BPP values. We only advertise 32-bit for simplicity and compositing
// compatibility. 24-bit has unaligned pitch issues; 16-bit requires color conversion.
const VBE_DISPI_BPP_32: u16 = 32;

// ============================================================================
// PCI Configuration Space
// ============================================================================

/// PCI config address register (write here to select bus/slot/func/offset)
const PCI_CONFIG_ADDRESS: u16 = 0xCF8;

/// PCI config data register (read/write the selected dword)
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// BGA PCI vendor ID (Bochs/QEMU virtual device)
const BGA_PCI_VENDOR_ID: u16 = 0x1234;

/// BGA PCI device ID
const BGA_PCI_DEVICE_ID: u16 = 0x1111;

/// PCI config space offset for BAR0 (Base Address Register 0)
const PCI_BAR0_OFFSET: u8 = 0x10;

/// Mask to extract the memory base address from a memory BAR
/// Bits [3:0] contain type flags; mask them away.
const PCI_MEM_BAR_ADDR_MASK: u32 = 0xFFFF_FFF0;

/// Memory BAR type field: bits [2:1]
///   00 = 32-bit BAR
///   10 = 64-bit BAR (address spans BAR0 + BAR1)
const PCI_MEM_BAR_TYPE_MASK:  u32 = 0x06;
const PCI_MEM_BAR_TYPE_32:    u32 = 0x00;
const PCI_MEM_BAR_TYPE_64:    u32 = 0x04;

/// Bit 0 of a BAR: 0 = memory bar, 1 = I/O bar
const PCI_BAR_IO_BIT: u32 = 0x01;

// ============================================================================
// Supported Video Modes
// ============================================================================

/// Maximum number of modes in the static table.
/// Re-exported from `atom_abi` so userspace and kernel always agree.
pub use atom_abi::VIDEO_MAX_MODES as MAX_VIDEO_MODES;

/// Represents a single display mode.
/// All modes use 32 BPP (BGRA 8-8-8-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct VideoMode {
    /// Horizontal resolution in pixels (must be divisible by 8 for BGA)
    pub width:  u16,
    /// Vertical resolution in pixels
    pub height: u16,
    /// Bits per pixel (always 32 for now)
    pub bpp:    u8,
    /// Refresh rate hint (informational, BGA does not control refresh)
    pub refresh_rate: u8,
}

impl VideoMode {
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height, bpp: 32, refresh_rate: 60 }
    }

    /// Bytes per pixel (bpp / 8)
    #[inline]
    pub fn bytes_per_pixel(&self) -> u32 {
        (self.bpp as u32 + 7) / 8
    }

    /// Pitch in bytes (bytes_per_scanline). BGA aligns to pixels; no additional padding.
    #[inline]
    pub fn pitch(&self) -> u32 {
        self.width as u32 * self.bytes_per_pixel()
    }

    /// Total framebuffer size in bytes
    #[inline]
    pub fn framebuffer_size(&self) -> usize {
        self.pitch() as usize * self.height as usize
    }
}

/// Static table of supported video modes.
/// These are standard resolutions known to work with QEMU's BGA device.
/// Ordered from smallest to largest for UI presentation.
///
/// Design choice: We enumerate a fixed set rather than querying GETCAPS because:
///   1. GETCAPS returns the maximum, not a list of discrete supported modes.
///   2. BGA supports any resolution up to the maximum (it's a virtual device).
///   3. Listing standard modes improves UX in the display settings UI.
const SUPPORTED_MODES: [VideoMode; 12] = [
    VideoMode::new(640,  480),
    VideoMode::new(800,  600),
    VideoMode::new(1024, 600),
    VideoMode::new(1024, 768),
    VideoMode::new(1152, 864),
    VideoMode::new(1280, 720),
    VideoMode::new(1280, 800),
    VideoMode::new(1280, 1024),
    VideoMode::new(1360, 768),
    VideoMode::new(1440, 900),
    VideoMode::new(1600, 900),
    VideoMode::new(1920, 1080),
];

/// VESA VBE 2.0 Mode Info Block (real-mode BIOS structure, kept for ABI compatibility
/// and future use when a BIOS emulation shim is available).
///
/// In the current UEFI long-mode environment this structure is populated
/// synthetically from BGA capabilities rather than from real BIOS calls.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VbeModeInfo {
    /// Mode attributes: bit 7 = LFB available, bit 0 = mode supported
    pub mode_attributes:       u16,
    pub win_a_attributes:      u8,
    pub win_b_attributes:      u8,
    pub win_granularity:        u16,
    pub win_size:               u16,
    pub win_a_segment:          u16,
    pub win_b_segment:          u16,
    pub win_func_ptr:           u32,
    pub bytes_per_scan_line:    u16,
    // OEM extensions (VBE 1.2+)
    pub x_resolution:           u16,
    pub y_resolution:           u16,
    pub x_char_size:            u8,
    pub y_char_size:            u8,
    pub number_of_planes:       u8,
    pub bits_per_pixel:         u8,
    pub number_of_banks:        u8,
    /// Memory model: 4 = packed pixel, 6 = direct colour
    pub memory_model:           u8,
    pub bank_size:              u8,
    pub number_of_image_pages:  u8,
    pub reserved1:              u8,
    // Direct colour fields
    pub red_mask_size:          u8,
    pub red_field_position:     u8,
    pub green_mask_size:        u8,
    pub green_field_position:   u8,
    pub blue_mask_size:         u8,
    pub blue_field_position:    u8,
    pub rsvd_mask_size:         u8,
    pub rsvd_field_position:    u8,
    pub direct_color_mode_info: u8,
    // VBE 2.0+ extensions
    pub phys_base_ptr:          u32,  // Physical address of LFB
    pub off_screen_mem_offset:  u32,
    pub off_screen_mem_size:    u16,
    pub reserved2:              [u8; 206],
}

impl VbeModeInfo {
    /// Construct a VbeModeInfo synthetically from a VideoMode and known LFB address.
    /// This allows the same info-block format to be used regardless of whether
    /// the mode was set via BGA or (in the future) real BIOS VBE calls.
    pub fn from_video_mode(mode: &VideoMode, lfb_phys: u32) -> Self {
        let mut info: VbeModeInfo = unsafe { core::mem::zeroed() };

        // Bit 0: mode supported; Bit 7: LFB available; Bit 4: graphics mode
        info.mode_attributes = 0b1001_0001;
        info.bytes_per_scan_line = mode.pitch() as u16;
        info.x_resolution = mode.width;
        info.y_resolution = mode.height;
        info.x_char_size = 8;
        info.y_char_size = 16;
        info.number_of_planes = 1;
        info.bits_per_pixel = mode.bpp;
        info.number_of_banks = 1;
        info.memory_model = 6;     // Direct colour
        info.number_of_image_pages = 0;
        info.reserved1 = 1;
        // 32-bit BGRA colour fields
        info.blue_mask_size      = 8; info.blue_field_position  = 0;
        info.green_mask_size     = 8; info.green_field_position = 8;
        info.red_mask_size       = 8; info.red_field_position   = 16;
        info.rsvd_mask_size      = 8; info.rsvd_field_position  = 24;
        info.direct_color_mode_info = 0;
        info.phys_base_ptr = lfb_phys;

        info
    }
}

// ============================================================================
// Driver State
// ============================================================================

/// Kernel-side state record for the BGA device.
struct BgaState {
    /// True if BGA hardware was detected and a valid version confirmed.
    available:      bool,
    /// The hardware version register value (e.g., 0xB0C5).
    version:        u16,
    /// Physical address of the Linear Framebuffer (from PCI BAR0).
    /// Zero if BGA is not available.
    lfb_phys:       usize,
    /// Maximum framebuffer size in bytes (from PCI BAR0 sizing).
    lfb_size:       usize,
    /// Currently active video mode.
    current_mode:   VideoMode,
    /// True if a mode has been set via set_video_mode().
    mode_active:    bool,
}

static BGA_STATE: Mutex<BgaState> = Mutex::new(BgaState {
    available:    false,
    version:      0,
    lfb_phys:     0,
    lfb_size:     0,
    current_mode: VideoMode { width: 0, height: 0, bpp: 32, refresh_rate: 60 },
    mode_active:  false,
});

const LOG_ORIGIN: &str = "bga";

// ============================================================================
// Raw I/O Helpers
// ============================================================================

/// Write to a VBE DISPI register via I/O ports.
///
/// # Safety
/// Caller must hold BGA_STATE lock and be executing in ring 0.
#[inline]
unsafe fn write_register(index: u16, value: u16) {
    crate::arch::outw(VBE_DISPI_PORT_INDEX, index);
    crate::arch::outw(VBE_DISPI_PORT_DATA, value);
}

/// Read from a VBE DISPI register via I/O ports.
///
/// # Safety
/// Caller must hold BGA_STATE lock and be executing in ring 0.
#[inline]
unsafe fn read_register(index: u16) -> u16 {
    crate::arch::outw(VBE_DISPI_PORT_INDEX, index);
    crate::arch::inw(VBE_DISPI_PORT_DATA)
}

// ============================================================================
// PCI Configuration Space Access
// ============================================================================

/// Construct a PCI config space address for the given bus/slot/func/offset.
///
/// Format (32 bits):
///   Bit 31:    Enable bit (always 1)
///   Bits 23:16 Bus number
///   Bits 15:11 Slot (device) number
///   Bits 10:8  Function number
///   Bits 7:2   Register offset (aligned to 4 bytes, i.e., offset >> 2)
///   Bits 1:0   Always 0
#[inline]
fn pci_config_addr(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000u32
        | ((bus  as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Read a 32-bit dword from PCI configuration space.
///
/// # Safety
/// Port I/O; must run in ring 0.
unsafe fn pci_config_read32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    let addr = pci_config_addr(bus, slot, func, offset);
    crate::arch::outl(PCI_CONFIG_ADDRESS, addr);
    crate::arch::inl(PCI_CONFIG_DATA)
}

/// Read a 16-bit word from PCI configuration space.
///
/// # Safety
/// Port I/O; must run in ring 0.
unsafe fn pci_config_read16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let dword = pci_config_read32(bus, slot, func, offset & 0xFC);
    let shift = (offset & 2) as u32 * 8;
    ((dword >> shift) & 0xFFFF) as u16
}

/// Scan the PCI bus for a device matching the given vendor and device IDs.
///
/// Returns (bus, slot, function) on success.
///
/// Scans bus 0 only (single-segment, flat topology). This is sufficient for
/// QEMU where BGA is always on bus 0. A production implementation would use
/// recursive PCI enumeration or ACPI MCFG for PCIe.
fn pci_find_device(vendor_id: u16, device_id: u16) -> Option<(u8, u8, u8)> {
    for slot in 0u8..32 {
        // Check function 0 first; if multi-function (header type bit 7), scan all.
        let header_type = unsafe {
            let raw = pci_config_read32(0, slot, 0, 0x0C);
            ((raw >> 16) & 0xFF) as u8
        };

        let max_func = if header_type & 0x80 != 0 { 8u8 } else { 1u8 };

        for func in 0..max_func {
            let vid_did = unsafe { pci_config_read32(0, slot, func, 0x00) };
            if vid_did == 0xFFFF_FFFF {
                // Slot/function not present
                continue;
            }
            let vid = (vid_did & 0xFFFF) as u16;
            let did = ((vid_did >> 16) & 0xFFFF) as u16;

            if vid == vendor_id && did == device_id {
                return Some((0, slot, func));
            }
        }
    }
    None
}

/// Read the 64-bit physical base address from BAR0 (supports both 32- and 64-bit BARs).
///
/// Memory BAR layout:
///   Bits [0]:   I/O space indicator (0 = memory, 1 = I/O)
///   Bits [2:1]: Type (00 = 32-bit, 10 = 64-bit, 01 = reserved)
///   Bit  [3]:   Prefetchable
///   Bits [31:4]: Base address (lower 28 bits of 32-bit; or lower 28 bits of 64-bit lower half)
///
/// For 64-bit BARs, BAR1 contains the upper 32 bits of the address.
fn pci_read_bar0_addr(bus: u8, slot: u8, func: u8) -> Option<usize> {
    let bar0 = unsafe { pci_config_read32(bus, slot, func, PCI_BAR0_OFFSET) };

    if bar0 == 0 || bar0 == 0xFFFF_FFFF {
        log_warn!(LOG_ORIGIN, "BGA BAR0 is zero or unset (0x{:X}) – firmware may not have assigned it", bar0);
        return None;
    }

    // Bit 0 = 1 means I/O bar; we need a memory bar.
    if bar0 & PCI_BAR_IO_BIT != 0 {
        log_error!(LOG_ORIGIN, "BGA BAR0 is an I/O bar (unexpected)");
        return None;
    }

    let bar_type = bar0 & PCI_MEM_BAR_TYPE_MASK;

    let phys_addr: usize = match bar_type {
        PCI_MEM_BAR_TYPE_32 => {
            // Standard 32-bit memory bar
            (bar0 & PCI_MEM_BAR_ADDR_MASK) as usize
        }
        PCI_MEM_BAR_TYPE_64 => {
            // 64-bit memory bar: base address spans BAR0 (lower 32 bits) and BAR1 (upper 32 bits)
            let bar0_lo = (bar0 & PCI_MEM_BAR_ADDR_MASK) as u64;
            let bar1    = unsafe {
                pci_config_read32(bus, slot, func, PCI_BAR0_OFFSET + 4) as u64
            };
            let addr64 = bar0_lo | (bar1 << 32);
            // Verify the address fits in the kernel's addressable space
            if addr64 > usize::MAX as u64 {
                log_error!(LOG_ORIGIN, "BGA BAR0 64-bit address 0x{:X} exceeds addressable range", addr64);
                return None;
            }
            addr64 as usize
        }
        _ => {
            log_error!(LOG_ORIGIN, "BGA BAR0 has reserved type field 0x{:X}", bar_type);
            return None;
        }
    };

    if phys_addr == 0 {
        log_warn!(LOG_ORIGIN, "BGA BAR0 physical address is 0; firmware did not assign it");
        return None;
    }

    Some(phys_addr)
}

/// Determine the size of a memory BAR by writing all-ones and reading back.
///
/// This is the standard PCI BAR sizing procedure:
///   1. Save the current BAR value.
///   2. Write 0xFFFFFFFF to the BAR.
///   3. Read back and mask the type bits.
///   4. Compute size = ~(value & mask) + 1.
///   5. Restore the original BAR value.
///
/// WARNING: This procedure momentarily disables the device's MMIO window.
/// It must only be called during early initialization when no other core
/// is accessing the BGA device.
#[allow(dead_code)]
unsafe fn pci_size_bar0(bus: u8, slot: u8, func: u8) -> usize {
    // Step 1: Save original value
    let orig = pci_config_read32(bus, slot, func, PCI_BAR0_OFFSET);

    // Step 2: Write all-ones
    let addr = pci_config_addr(bus, slot, func, PCI_BAR0_OFFSET);
    crate::arch::outl(PCI_CONFIG_ADDRESS, addr);
    crate::arch::outl(PCI_CONFIG_DATA, 0xFFFF_FFFFu32);

    // Step 3: Read back
    crate::arch::outl(PCI_CONFIG_ADDRESS, addr);
    let size_mask = crate::arch::inl(PCI_CONFIG_DATA);

    // Step 4: Restore original
    crate::arch::outl(PCI_CONFIG_ADDRESS, addr);
    crate::arch::outl(PCI_CONFIG_DATA, orig);

    // Compute size from the readback mask (ignoring BAR type bits)
    let masked = size_mask & PCI_MEM_BAR_ADDR_MASK;
    if masked == 0 {
        return 0;
    }
    (!(masked) + 1) as usize
}

// ============================================================================
// Initialization and Detection
// ============================================================================

/// Probe for the BGA device and initialize the driver state.
///
/// This function must be called once, early in kernel init, before any other
/// BGA functions. It:
///   1. Reads the VBE DISPI ID register via I/O ports.
///   2. Validates the version (>= 0xB0C5).
///   3. Locates the BGA PCI device to obtain the LFB physical address.
///   4. Maps the LFB into the kernel address space.
///
/// Returns `true` if BGA is available and initialized successfully.
pub fn init() -> bool {
    log_info!(LOG_ORIGIN, "Probing Bochs Graphics Adapter (VBE DISPI)...");

    // Step 1: Check the version register
    let version = unsafe { read_register(VBE_DISPI_INDEX_ID) };
    log_info!(LOG_ORIGIN, "VBE DISPI ID register: 0x{:04X}", version);

    if version < VBE_DISPI_ID5 {
        log_warn!(
            LOG_ORIGIN,
            "BGA not available or too old: version=0x{:04X} (need >= 0x{:04X})",
            version,
            VBE_DISPI_ID5
        );
        return false;
    }

    // Step 2: Locate BGA on the PCI bus
    let pci_location = match pci_find_device(BGA_PCI_VENDOR_ID, BGA_PCI_DEVICE_ID) {
        Some(loc) => {
            log_info!(LOG_ORIGIN, "BGA PCI device found at bus={} slot={} func={}", loc.0, loc.1, loc.2);
            loc
        }
        None => {
            log_warn!(LOG_ORIGIN, "BGA PCI device not found on bus scan");
            return false;
        }
    };

    // Step 3: Read BAR0 for the LFB physical address
    let lfb_phys = match pci_read_bar0_addr(pci_location.0, pci_location.1, pci_location.2) {
        Some(addr) => {
            log_info!(LOG_ORIGIN, "BGA LFB physical address: 0x{:X}", addr);
            addr
        }
        None => {
            log_error!(LOG_ORIGIN, "Failed to read BGA LFB address from PCI BAR0");
            return false;
        }
    };

    // Step 4: Determine maximum framebuffer size.
    // We use 16MB as a conservative default (covers 1920x1200x32 = ~8.8MB).
    // The actual BGA device in QEMU provides 16MB of video memory by default.
    let lfb_size = 16 * 1024 * 1024usize;

    // Step 5: Map the LFB into the kernel's address space so the kernel
    // can write to it for the double-buffer blit path.
    // Use cache-disable (write-combining would be ideal but requires MTRR/PAT setup).
    let map_result = vm::map_framebuffer(lfb_phys as u64, lfb_size);
    if !map_result {
        log_warn!(
            LOG_ORIGIN,
            "BGA LFB mapping partially failed; continuing with whatever was mapped"
        );
    }

    // Step 6: Update driver state
    let mut state = BGA_STATE.lock();
    state.available    = true;
    state.version      = version;
    state.lfb_phys     = lfb_phys;
    state.lfb_size     = lfb_size;
    state.mode_active  = false;

    log_info!(
        LOG_ORIGIN,
        "BGA driver initialized: version=0x{:04X}, LFB=0x{:X}, size={}MB",
        version,
        lfb_phys,
        lfb_size / (1024 * 1024)
    );

    true
}

/// Returns `true` if the BGA driver has been successfully initialized.
pub fn is_available() -> bool {
    BGA_STATE.lock().available
}

// ============================================================================
// Video Mode Management
// ============================================================================

/// Validate a requested video mode against hardware constraints.
///
/// BGA constraints:
///   - Width must be divisible by 8 (hardware limitation)
///   - Width and height must be > 0
///   - The framebuffer must fit in the mapped LFB region
///   - Only 32 BPP is supported by this driver
fn validate_mode(width: u16, height: u16, bpp: u8, lfb_size: usize) -> Result<(), BgaError> {
    if width == 0 || height == 0 {
        return Err(BgaError::InvalidResolution);
    }
    if width % 8 != 0 {
        return Err(BgaError::WidthMisaligned);
    }
    if bpp != 32 {
        return Err(BgaError::UnsupportedBpp);
    }
    let required = (width as usize) * (height as usize) * ((bpp as usize + 7) / 8);
    if required > lfb_size {
        return Err(BgaError::ExceedsLfbSize);
    }
    Ok(())
}

/// Set the video mode on the BGA device.
///
/// This is the primary interface for changing the display resolution.
///
/// # Parameters
/// - `width`:  Horizontal resolution (must be divisible by 8)
/// - `height`: Vertical resolution
/// - `bpp`:    Bits per pixel (must be 32)
/// - `clear`:  If `true`, the VRAM is cleared by the hardware on mode switch
///
/// # Safety Considerations
/// - Acquires BGA_STATE lock for the duration.
/// - Issues port I/O to hardware.
/// - The screen will briefly show garbage during the disable/enable sequence.
pub fn set_video_mode(width: u16, height: u16, bpp: u8, clear: bool) -> Result<(), BgaError> {
    let mut state = BGA_STATE.lock();

    if !state.available {
        return Err(BgaError::NotAvailable);
    }

    validate_mode(width, height, bpp, state.lfb_size)?;

    log_info!(
        LOG_ORIGIN,
        "Setting video mode: {}x{}x{} (clear={})",
        width, height, bpp, clear
    );

    // Mode-setting sequence per VBE DISPI specification:
    unsafe {
        // 1. Disable VBE to allow register writes
        write_register(VBE_DISPI_INDEX_ENABLE, VBE_DISPI_DISABLED);

        // 2. Write resolution and colour depth
        write_register(VBE_DISPI_INDEX_XRES, width);
        write_register(VBE_DISPI_INDEX_YRES, height);
        write_register(VBE_DISPI_INDEX_BPP, bpp as u16);

        // 3. Virtual framebuffer = physical (no scrolling for now)
        write_register(VBE_DISPI_INDEX_VIRT_WIDTH,  width);
        write_register(VBE_DISPI_INDEX_VIRT_HEIGHT, height);

        // 4. Reset scroll offsets
        write_register(VBE_DISPI_INDEX_X_OFFSET, 0);
        write_register(VBE_DISPI_INDEX_Y_OFFSET, 0);

        // 5. Enable VBE with LFB; optionally skip VRAM clear for performance
        let enable_flags = VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED
            | if clear { 0 } else { VBE_DISPI_NOCLEARMEM };
        write_register(VBE_DISPI_INDEX_ENABLE, enable_flags);
    }

    // 6. Verify the mode was accepted by reading back XRES/YRES
    let actual_xres = unsafe { read_register(VBE_DISPI_INDEX_XRES) };
    let actual_yres = unsafe { read_register(VBE_DISPI_INDEX_YRES) };

    if actual_xres != width || actual_yres != height {
        log_error!(
            LOG_ORIGIN,
            "Mode verification failed: requested {}x{} but got {}x{}",
            width, height, actual_xres, actual_yres
        );
        return Err(BgaError::HardwareRejected);
    }

    // 7. Update state
    state.current_mode = VideoMode { width, height, bpp, refresh_rate: 60 };
    state.mode_active = true;

    log_info!(
        LOG_ORIGIN,
        "Mode set successfully: {}x{}x{}, LFB=0x{:X}, pitch={}B",
        width, height, bpp,
        state.lfb_phys,
        width as u32 * (bpp as u32 / 8)
    );

    Ok(())
}

/// Dynamically update the resolution (e.g., in response to a QEMU window resize).
///
/// Preserves the existing VRAM content where possible (NOCLEARMEM flag).
/// After this call, the caller is responsible for redrawing the framebuffer
/// since the new resolution may differ from the old one.
pub fn update_resolution(new_width: u16, new_height: u16) -> Result<(), BgaError> {
    // Validate alignment first
    if new_width % 8 != 0 {
        return Err(BgaError::WidthMisaligned);
    }
    // Preserve VRAM content (no clear) since the compositor will repaint.
    set_video_mode(new_width, new_height, 32, false)
}

// ============================================================================
// VESA Mode Enumeration
// ============================================================================

/// Query hardware capabilities: returns the maximum supported resolution.
///
/// Uses the VBE_DISPI_GETCAPS flag which causes XRES/YRES reads to return
/// the hardware maximum rather than the current setting.
pub fn get_max_resolution() -> Option<(u16, u16)> {
    let state = BGA_STATE.lock();
    if !state.available {
        return None;
    }

    unsafe {
        // Enable GETCAPS flag; do not change mode
        let current_enable = read_register(VBE_DISPI_INDEX_ENABLE);
        write_register(VBE_DISPI_INDEX_ENABLE, current_enable | VBE_DISPI_GETCAPS);

        let max_xres = read_register(VBE_DISPI_INDEX_XRES);
        let max_yres = read_register(VBE_DISPI_INDEX_YRES);

        // Restore ENABLE register
        write_register(VBE_DISPI_INDEX_ENABLE, current_enable);

        Some((max_xres, max_yres))
    }
}

/// Copy all supported video modes into the provided buffer.
/// Returns the number of modes written.
pub fn get_supported_modes(buf: &mut [VideoMode]) -> usize {
    let count = SUPPORTED_MODES.len().min(buf.len());
    buf[..count].copy_from_slice(&SUPPORTED_MODES[..count]);
    count
}

/// Total count of available video modes.
pub fn mode_count() -> usize {
    SUPPORTED_MODES.len()
}

/// Find the best matching mode for the requested resolution and BPP.
///
/// This implements the `find_best_mode` function described in the spec.
/// The algorithm:
///   1. Try to find an exact match.
///   2. If not found, find the closest mode by total pixel difference.
///   3. Only return modes that support linear framebuffer (all BGA modes do).
///
/// This function is analogous to what INT 0x10/AX=0x4F01 would iterate through
/// in a real BIOS VBE implementation.
pub fn find_best_mode(target_width: u16, target_height: u16, target_bpp: u8) -> Option<&'static VideoMode> {
    // Only 32 BPP is currently supported
    if target_bpp != 32 {
        return None;
    }

    // Exact match first
    if let Some(m) = SUPPORTED_MODES.iter().find(|m| m.width == target_width && m.height == target_height) {
        return Some(m);
    }

    // Closest approximation by total pixel area difference
    let target_pixels = (target_width as u64) * (target_height as u64);
    SUPPORTED_MODES.iter().min_by_key(|m| {
        let mode_pixels = (m.width as u64) * (m.height as u64);
        if mode_pixels > target_pixels {
            mode_pixels - target_pixels
        } else {
            target_pixels - mode_pixels
        }
    })
}

// ============================================================================
// Current State Accessors
// ============================================================================

/// Get current video mode information as a tuple:
/// `(lfb_phys, width, height, pitch, bytes_per_pixel)`
///
/// Returns `None` if no mode has been set or BGA is not available.
pub fn get_current_mode_info() -> Option<(usize, u16, u16, u32, u32)> {
    let state = BGA_STATE.lock();
    if !state.available || !state.mode_active {
        return None;
    }
    let m = &state.current_mode;
    let pitch = m.pitch();
    let bpp_bytes = m.bytes_per_pixel();
    Some((state.lfb_phys, m.width, m.height, pitch, bpp_bytes))
}

/// Get the physical address of the Linear Framebuffer.
pub fn get_lfb_phys() -> Option<usize> {
    let state = BGA_STATE.lock();
    if !state.available {
        None
    } else {
        Some(state.lfb_phys)
    }
}

/// Get the current VideoMode.
pub fn get_current_mode() -> Option<VideoMode> {
    let state = BGA_STATE.lock();
    if !state.mode_active {
        None
    } else {
        Some(state.current_mode)
    }
}

// ============================================================================
// Kernel Video Info (Published for Syscalls)
// ============================================================================

/// Packed structure published to userspace via SYS_GET_CURRENT_VIDEO_MODE.
/// All fields are 64-bit to avoid alignment issues in the syscall ABI.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KernelVideoInfo {
    /// Physical address of the Linear Framebuffer
    pub lfb_phys:         u64,
    /// Horizontal resolution in pixels
    pub width:            u32,
    /// Vertical resolution in pixels
    pub height:           u32,
    /// Bytes per scanline (pitch)
    pub pitch:            u32,
    /// Bytes per pixel
    pub bytes_per_pixel:  u32,
    /// BGA driver available (1) or not (0)
    pub bga_available:    u32,
    /// Mode is active (1) or not (0)
    pub mode_active:      u32,
}

impl KernelVideoInfo {
    pub fn from_state() -> Self {
        let state = BGA_STATE.lock();
        if !state.available {
            return Self::default();
        }
        let m = &state.current_mode;
        Self {
            lfb_phys:        state.lfb_phys as u64,
            width:           m.width as u32,
            height:          m.height as u32,
            pitch:           m.pitch(),
            bytes_per_pixel: m.bytes_per_pixel(),
            bga_available:   1,
            mode_active:     if state.mode_active { 1 } else { 0 },
        }
    }
}
