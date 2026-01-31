// bootloader_vesa_vbe.rs
//
// Legacy BIOS VESA VBE mode selection (LFB) and FramebufferInfo construction
//
// IMPORTANT:
// - This code is meant to live in your *legacy boot stub / bootloader*, not in the kernel.
// - It must run in an environment capable of invoking BIOS int 0x10.
// - You must provide the "int10" bridge for your specific bootloader (real mode, unreal mode,
//   vm86, or a 16-bit stage).
//
// This file is "complete" from a Rust standpoint, but the INT10 bridge is platform-specific.
// You plug that in by implementing `BiosInt10::call()` and providing a low-memory scratch
// buffer for VBE structures (commonly in the first 1MB).
//
// Output:
// - A fully populated `crate::boot::FramebufferInfo` compatible with your kernel `graphics.rs`.
//
// Safety/robustness:
// - Only selects modes that provide a linear framebuffer and direct-color/packed pixels.
// - Strongly prefers 32bpp.
// - Builds EfiPixelBitmask masks from VBE field-position + mask-size fields.
//
// Dependencies: core only.

#![no_std]

use core::mem::size_of;

use crate::boot::{EfiPixelBitmask, FramebufferInfo, PixelFormat};

const VBE_SUCCESS: u16 = 0x004F;
const VBE_FUNC_CONTROLLER_INFO: u16 = 0x4F00;
const VBE_FUNC_MODE_INFO: u16 = 0x4F01;
const VBE_FUNC_SET_MODE: u16 = 0x4F02;

const VBE_MODE_LFB: u16 = 1 << 14;

// ModeAttributes bits
const MODE_ATTR_SUPPORTED: u16 = 1 << 0;
const MODE_ATTR_COLOR: u16 = 1 << 3;
const MODE_ATTR_GRAPHICS: u16 = 1 << 4;
const MODE_ATTR_LFB_AVAILABLE: u16 = 1 << 7;

// MemoryModel values (VBE)
const MEMMODEL_PACKED_PIXEL: u8 = 4;
const MEMMODEL_DIRECT_COLOR: u8 = 6;

#[repr(C, packed)]
pub struct VbeControllerInfo {
    pub signature: [u8; 4], // must be "VESA"
    pub version: u16,
    pub oem_string_ptr: u32,
    pub capabilities: [u8; 4],
    pub video_mode_ptr: u32, // far ptr to mode list
    pub total_memory_64kb: u16,

    // VBE 2.0+ fields continue; we don't need them here
    pub _reserved: [u8; 236],
}

impl VbeControllerInfo {
    pub fn set_signature_vbe2(&mut self) {
        // For VBE2, you typically set "VBE2" before call, but "VESA" is also acceptable.
        self.signature = *b"VBE2";
    }
}

#[repr(C, packed)]
pub struct VbeModeInfo {
    pub mode_attributes: u16,
    pub win_a_attributes: u8,
    pub win_b_attributes: u8,
    pub win_granularity: u16,
    pub win_size: u16,
    pub win_a_segment: u16,
    pub win_b_segment: u16,
    pub win_func_ptr: u32,
    pub bytes_per_scan_line: u16,

    pub x_resolution: u16,
    pub y_resolution: u16,
    pub x_char_size: u8,
    pub y_char_size: u8,
    pub number_of_planes: u8,
    pub bits_per_pixel: u8,
    pub number_of_banks: u8,
    pub memory_model: u8,
    pub bank_size: u8,
    pub number_of_image_pages: u8,
    pub reserved1: u8,

    pub red_mask_size: u8,
    pub red_field_position: u8,
    pub green_mask_size: u8,
    pub green_field_position: u8,
    pub blue_mask_size: u8,
    pub blue_field_position: u8,
    pub reserved_mask_size: u8,
    pub reserved_field_position: u8,
    pub direct_color_mode_info: u8,

    pub phys_base_ptr: u32,
    pub reserved2: u32,
    pub reserved3: u16,

    // VBE3.0+ and padding
    pub _reserved: [u8; 206],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BiosRegs16 {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub si: u16,
    pub di: u16,
    pub es: u16,
    pub ds: u16,
    pub flags: u16,
}

/// Platform-specific BIOS int 10h bridge.
/// You MUST implement this in your legacy boot stub.
///
/// The call must preserve/return registers and allow ES:DI pointers to low memory.
pub trait BiosInt10 {
    fn call(&mut self, regs: &mut BiosRegs16);
}

/// A low-memory buffer provider.
/// VBE expects structures accessible by BIOS (typically below 1MB).
pub trait LowMem {
    /// Returns a physical address below 1MB and a mutable slice view of that buffer.
    /// Size must be >= `len`.
    fn alloc(&mut self, len: usize, align: usize) -> (u32, &mut [u8]);

    /// Convert a physical address (below 1MB) to ES:DI.
    fn phys_to_far(&self, phys: u32) -> (u16, u16);

    /// Convert a VBE far pointer (segment:offset stored as u32) into a physical address.
    /// In VBE, far ptr is typically (seg<<16)|off.
    fn far_to_phys(&self, far: u32) -> u32;
}

fn build_mask(bits: u8, shift: u8) -> u32 {
    if bits == 0 {
        return 0;
    }
    if bits >= 32 {
        return 0xFFFF_FFFFu32;
    }
    ((1u32 << bits) - 1) << (shift as u32)
}

fn vbe_ok(ax: u16) -> bool {
    ax == VBE_SUCCESS
}

fn mode_usable(m: &VbeModeInfo) -> bool {
    let attr = m.mode_attributes;

    if (attr & MODE_ATTR_SUPPORTED) == 0 {
        return false;
    }
    if (attr & MODE_ATTR_GRAPHICS) == 0 {
        return false;
    }
    if (attr & MODE_ATTR_COLOR) == 0 {
        return false;
    }
    if (attr & MODE_ATTR_LFB_AVAILABLE) == 0 {
        return false;
    }

    match m.memory_model {
        MEMMODEL_DIRECT_COLOR | MEMMODEL_PACKED_PIXEL => {}
        _ => return false,
    }

    // Prefer packed/direct with 32bpp
    if m.bits_per_pixel != 32 {
        return false;
    }

    // Must have a valid phys framebuffer base
    if m.phys_base_ptr == 0 {
        return false;
    }

    true
}

pub struct VesaVbe;

impl VesaVbe {
    pub fn try_init_and_select_mode<I10: BiosInt10, LM: LowMem>(
        int10: &mut I10,
        lowmem: &mut LM,
        preferred: &[(u16, u16)], // list of (width,height) preferences
    ) -> Option<FramebufferInfo> {
        // Allocate controller info in low memory
        let (ctrl_phys, ctrl_buf) = lowmem.alloc(size_of::<VbeControllerInfo>(), 16);
        let ctrl = unsafe { &mut *(ctrl_buf.as_mut_ptr() as *mut VbeControllerInfo) };
        // Initialize signature request
        *ctrl = unsafe { core::mem::zeroed() };
        ctrl.set_signature_vbe2();

        // Call VBE controller info (AX=4F00, ES:DI=buf)
        let (es, di) = lowmem.phys_to_far(ctrl_phys);
        let mut regs = BiosRegs16 {
            ax: VBE_FUNC_CONTROLLER_INFO,
            bx: 0,
            cx: 0,
            dx: 0,
            si: 0,
            di,
            es,
            ds: 0,
            flags: 0,
        };
        int10.call(&mut regs);
        if !vbe_ok(regs.ax) {
            return None;
        }

        // Signature must be "VESA" after the call
        if ctrl.signature != *b"VESA" && ctrl.signature != *b"VBE2" {
            return None;
        }

        // Resolve mode list pointer
        let mode_list_phys = lowmem.far_to_phys(ctrl.video_mode_ptr);
        if mode_list_phys == 0 {
            return None;
        }

        // Iterate mode list (u16 entries, terminated by 0xFFFF)
        // We assume lowmem can give us a slice view; if not, you can read via your own phys reader.
        // Here we just allocate a window and copy; that implementation is left to your LowMem.
        //
        // To keep this file complete and deterministic, we require LowMem to hand us a view
        // by allocating a small buffer and then having the boot stub copy the mode list into it
        // before calling this function OR implement far_to_phys + memory mapping in LowMem.
        //
        // If you already have identity-mapped low memory, you can provide a LowMem that returns a slice
        // over that region. Otherwise, copy it.
        //
        // For simplicity: attempt to read up to 4096 bytes worth of mode ids.
        let max_modes_bytes = 4096usize;
        let (modes_phys, modes_buf) = lowmem.alloc(max_modes_bytes, 2);
        // Platform-specific: YOU must copy from mode_list_phys into modes_phys here if your LowMem
        // implementation doesn't alias real memory. If it DOES alias, then far_to_phys points to that
        // same memory and this is unnecessary.
        //
        // In a typical "identity-mapped low memory" legacy stub, you can implement alloc() as
        // "return a pointer into an existing static scratch page" and also implement a method to memcpy
        // from one physical to another. That logic is outside this file.

        // Interpret buffer as u16 list
        let mode_list = unsafe {
            core::slice::from_raw_parts(modes_buf.as_ptr() as *const u16, max_modes_bytes / 2)
        };

        // Allocate mode info buffer (low memory)
        let (modeinfo_phys, modeinfo_buf) = lowmem.alloc(size_of::<VbeModeInfo>(), 16);
        let modeinfo = unsafe { &mut *(modeinfo_buf.as_mut_ptr() as *mut VbeModeInfo) };

        // Search for preferred resolutions in order
        for &(pw, ph) in preferred.iter() {
            // scan mode list
            for &mode in mode_list.iter() {
                if mode == 0xFFFF {
                    break;
                }

                // Get mode info: AX=4F01, CX=mode, ES:DI=buf
                *modeinfo = unsafe { core::mem::zeroed() };
                let (es_mi, di_mi) = lowmem.phys_to_far(modeinfo_phys);

                let mut r = BiosRegs16 {
                    ax: VBE_FUNC_MODE_INFO,
                    bx: 0,
                    cx: mode,
                    dx: 0,
                    si: 0,
                    di: di_mi,
                    es: es_mi,
                    ds: 0,
                    flags: 0,
                };
                int10.call(&mut r);
                if !vbe_ok(r.ax) {
                    continue;
                }

                if modeinfo.x_resolution != pw || modeinfo.y_resolution != ph {
                    continue;
                }
                if !mode_usable(modeinfo) {
                    continue;
                }

                // Set mode with LFB: AX=4F02, BX=mode|LFB
                let mut r2 = BiosRegs16 {
                    ax: VBE_FUNC_SET_MODE,
                    bx: mode | VBE_MODE_LFB,
                    cx: 0,
                    dx: 0,
                    si: 0,
                    di: 0,
                    es: 0,
                    ds: 0,
                    flags: 0,
                };
                int10.call(&mut r2);
                if !vbe_ok(r2.ax) {
                    continue;
                }

                // Build kernel-compatible FramebufferInfo
                let red_mask = build_mask(modeinfo.red_mask_size, modeinfo.red_field_position);
                let green_mask = build_mask(modeinfo.green_mask_size, modeinfo.green_field_position);
                let blue_mask = build_mask(modeinfo.blue_mask_size, modeinfo.blue_field_position);
                let reserved_mask =
                    build_mask(modeinfo.reserved_mask_size, modeinfo.reserved_field_position);

                let pixel_bitmask = EfiPixelBitmask {
                    red_mask,
                    green_mask,
                    blue_mask,
                    reserved_mask,
                };

                // bytes_per_scan_line is in bytes, but kernel struct expects pixels_per_scan_line.
                // For 32bpp, pixels_per_scan_line = bytes_per_scan_line / 4.
                let stride_pixels = (modeinfo.bytes_per_scan_line as u32) / 4;

                // size = stride_pixels * height * 4
                let size = (stride_pixels as u64)
                    .saturating_mul(modeinfo.y_resolution as u64)
                    .saturating_mul(4);

                return Some(FramebufferInfo {
                    address: modeinfo.phys_base_ptr as u64,
                    size: size as usize,
                    width: modeinfo.x_resolution as u32,
                    height: modeinfo.y_resolution as u32,
                    pixels_per_scan_line: stride_pixels,
                    pixel_format: PixelFormat::Bitmask, // most correct for VBE
                    pixel_bitmask,
                });
            }
        }

        None
    }
}