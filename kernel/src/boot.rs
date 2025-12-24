//! Boot-time data structures shared across architectures.
//!
//! This module intentionally contains **no** firmware-specific logic. It only
//! defines the neutral data passed from the platform boot stub into the kernel
//! proper.

pub const EFI_CONVENTIONAL_MEMORY: u32 = 7;

#[repr(C)]
pub struct MemoryMap {
    pub buffer: *const u8,
    pub size: usize,
    pub descriptor_size: usize,
}

unsafe impl Send for MemoryMap {}
unsafe impl Sync for MemoryMap {}

impl MemoryMap {
    pub const fn new(buffer: *const u8, size: usize, descriptor_size: usize) -> Self {
        Self {
            buffer,
            size,
            descriptor_size,
        }
    }

    pub fn descriptors(&self) -> MemoryMapIter {
        MemoryMapIter {
            buffer: self.buffer,
            size: self.size,
            descriptor_size: self.descriptor_size,
            offset: 0,
        }
    }
}

pub struct MemoryMapIter {
    buffer: *const u8,
    size: usize,
    descriptor_size: usize,
    offset: usize,
}

impl Iterator for MemoryMapIter {
    type Item = &'static EfiMemoryDescriptor;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.size {
            return None;
        }

        unsafe {
            let desc_ptr = self.buffer.add(self.offset) as *const EfiMemoryDescriptor;
            self.offset += self.descriptor_size;
            Some(&*desc_ptr)
        }
    }
}

#[repr(C)]
pub struct EfiMemoryDescriptor {
    pub typ: u32,
    pub pad: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EfiPixelBitmask {
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub reserved_mask: u32,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb = 0,
    Bgr = 1,
    Bitmask = 2,
    BltOnly = 3,
    Unknown = 0xFFFF_FFFF,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FramebufferInfo {
    pub address: u64,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub pixels_per_scan_line: u32,
    pub pixel_format: PixelFormat,
    pub pixel_bitmask: EfiPixelBitmask,
}

impl FramebufferInfo {
    pub const fn empty() -> Self {
        Self {
            address: 0,
            size: 0,
            width: 0,
            height: 0,
            pixels_per_scan_line: 0,
            pixel_format: PixelFormat::Unknown,
            pixel_bitmask: EfiPixelBitmask {
                red_mask: 0,
                green_mask: 0,
                blue_mask: 0,
                reserved_mask: 0,
            },
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExecutableImage {
    pub ptr: *const u8,
    pub size: usize,
}

impl ExecutableImage {
    pub const fn empty() -> Self {
        Self {
            ptr: core::ptr::null(),
            size: 0,
        }
    }

    pub fn is_present(&self) -> bool {
        !self.ptr.is_null() && self.size > 0
    }
}

unsafe impl Send for ExecutableImage {}
unsafe impl Sync for ExecutableImage {}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum BootMethod {
    Uefi,
    Legacy,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum CpuArchitecture {
    X86_64,
    AArch64,
    Unknown,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct CpuInfo {
    pub vendor: [u8; 12],
    pub brand: [u8; 48],
    pub architecture: CpuArchitecture,
}

#[repr(C)]
pub struct BootInfo {
    pub memory_map: MemoryMap,
    pub framebuffer: FramebufferInfo,
    pub framebuffer_present: bool,
    pub verbose: bool,
    pub boot_method: BootMethod,
    pub cpu: CpuInfo,
    pub init_payload: ExecutableImage,
}

unsafe impl Send for BootInfo {}
unsafe impl Sync for BootInfo {}

impl BootInfo {
    pub const fn empty() -> Self {
        Self {
            memory_map: MemoryMap {
                buffer: core::ptr::null(),
                size: 0,
                descriptor_size: 0,
            },
            framebuffer: FramebufferInfo::empty(),
            framebuffer_present: false,
            verbose: false,
            boot_method: BootMethod::Uefi,
            cpu: CpuInfo {
                vendor: [0; 12],
                brand: [0; 48],
                architecture: CpuArchitecture::Unknown,
            },
            init_payload: ExecutableImage::empty(),
        }
    }
}
