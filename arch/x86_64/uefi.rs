// x86_64 UEFI Boot Entry and Firmware Handoff
//
// This module is the sole owner of the UEFI ABI surface. It retrieves the
// minimal data required by the kernel, builds a neutral `BootInfo` structure,
// and then transfers control to `kmain`.

use core::ffi::c_void;
use core::ptr;

use spin::Once;

use crate::boot::{
    BootInfo, BootMethod, CpuArchitecture, CpuInfo, ExecutableImage, FramebufferInfo, MemoryMap,
    PixelFormat, EfiMemoryDescriptor, EfiPixelBitmask,
};

extern "C" {
    fn kmain(boot_info: &'static BootInfo) -> !;
}

static BOOT_INFO_STORAGE: Once<BootInfo> = Once::new();

type EfiStatus = usize;
type EfiHandle = *mut c_void;

type EfiGetMemoryMap = extern "win64" fn(
    memory_map_size: *mut usize,
    memory_map: *mut EfiMemoryDescriptor,
    map_key: *mut usize,
    descriptor_size: *mut usize,
    descriptor_version: *mut u32,
) -> EfiStatus;

type EfiAllocatePool = extern "win64" fn(
    pool_type: u32,
    size: usize,
    buffer: *mut *mut c_void,
) -> EfiStatus;

type EfiFreePool = extern "win64" fn(buffer: *mut c_void) -> EfiStatus;

type EfiExitBootServices =
    extern "win64" fn(image_handle: EfiHandle, map_key: usize) -> EfiStatus;

type EfiWaitForEvent =
    extern "win64" fn(number_of_events: usize, events: *mut *mut c_void, index: *mut usize)
        -> EfiStatus;

type EfiStall = extern "win64" fn(microseconds: usize) -> EfiStatus;

type EfiLocateProtocol = extern "win64" fn(
    protocol: *const EfiGuid,
    registration: *mut c_void,
    interface: *mut *mut c_void,
) -> EfiStatus;

type EfiSetWatchdogTimer = extern "win64" fn(
    timeout: usize,
    watchdog_code: u64,
    data_size: usize,
    watchdog_data: *mut c_void,
) -> EfiStatus;

const EFI_SUCCESS: EfiStatus = 0;
const EFI_BUFFER_TOO_SMALL: EfiStatus = 0x8000_0000_0000_0005;
const EFI_INVALID_PARAMETER: EfiStatus = 0x8000_0000_0000_0002;
const EFI_LOADER_DATA: u32 = 2;

#[repr(C)]
struct EfiTableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    _reserved: u32,
}

#[repr(C)]
struct EfiGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct EfiSystemTable {
    hdr: EfiTableHeader,
    firmware_vendor: *const u16,
    firmware_revision: u32,
    console_in_handle: EfiHandle,
    con_in: *mut c_void,
    console_out_handle: EfiHandle,
    con_out: *mut c_void,
    standard_error_handle: EfiHandle,
    std_err: *mut c_void,
    runtime_services: *mut c_void,
    boot_services: *mut EfiBootServices,
    number_of_table_entries: usize,
    configuration_table: *mut c_void,
}

#[repr(C)]
struct EfiBootServices {
    hdr: EfiTableHeader,
    raise_tpl: usize,
    restore_tpl: usize,
    allocate_pages: usize,
    free_pages: usize,
    get_memory_map: EfiGetMemoryMap,
    allocate_pool: EfiAllocatePool,
    free_pool: EfiFreePool,
    create_event: usize,
    set_timer: usize,
    wait_for_event: EfiWaitForEvent,
    signal_event: usize,
    close_event: usize,
    check_event: usize,
    install_protocol_interface: usize,
    reinstall_protocol_interface: usize,
    uninstall_protocol_interface: usize,
    handle_protocol: usize,
    _reserved: usize,
    register_protocol_notify: usize,
    locate_handle: usize,
    locate_device_path: usize,
    install_configuration_table: usize,
    load_image: usize,
    start_image: usize,
    exit: usize,
    unload_image: usize,
    exit_boot_services: EfiExitBootServices,
    get_next_monotonic_count: usize,
    stall: EfiStall,
    set_watchdog_timer: EfiSetWatchdogTimer,
    connect_controller: usize,
    disconnect_controller: usize,
    open_protocol: usize,
    close_protocol: usize,
    open_protocol_information: usize,
    protocols_per_handle: usize,
    locate_handle_buffer: usize,
    locate_protocol: EfiLocateProtocol,
}

#[repr(C)]
struct EfiGraphicsOutputModeInformation {
    version: u32,
    horizontal_resolution: u32,
    vertical_resolution: u32,
    pixel_format: u32,
    pixel_information: EfiPixelBitmask,
    pixels_per_scan_line: u32,
}

#[repr(C)]
struct EfiGraphicsOutputProtocolMode {
    max_mode: u32,
    mode: u32,
    info: *const EfiGraphicsOutputModeInformation,
    size_of_info: usize,
    frame_buffer_base: u64,
    frame_buffer_size: usize,
}

#[repr(C)]
struct EfiGraphicsOutputProtocol {
    query_mode: usize,
    set_mode: usize,
    blt: usize,
    mode: *const EfiGraphicsOutputProtocolMode,
}

const GOP_GUID: EfiGuid = EfiGuid {
    data1: 0x9042A9DE,
    data2: 0x23DC,
    data3: 0x4A38,
    data4: [0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A],
};

fn get_cpu_vendor() -> [u8; 12] {
    let mut vendor = [0u8; 12];

    unsafe {
        let ebx: u32;
        let ecx: u32;
        let edx: u32;

        core::arch::asm!(
            "push rbx",
            "xor eax, eax",
            "cpuid",
            "mov {ebx_tmp:e}, ebx",
            "pop rbx",
            ebx_tmp = out(reg) ebx,
            out("ecx") ecx,
            out("edx") edx,
            out("eax") _,
        );

        vendor[0..4].copy_from_slice(&ebx.to_le_bytes());
        vendor[4..8].copy_from_slice(&edx.to_le_bytes());
        vendor[8..12].copy_from_slice(&ecx.to_le_bytes());
    }

    vendor
}

fn get_cpu_brand() -> [u8; 48] {
    let mut brand = [0u8; 48];

    unsafe {
        for i in 0..3 {
            let leaf: u32 = 0x8000_0002 + i;
            let eax: u32;
            let ebx: u32;
            let ecx: u32;
            let edx: u32;

            core::arch::asm!(
                "push rbx",
                "cpuid",
                "mov {ebx_tmp:e}, ebx",
                "pop rbx",
                inout("eax") leaf => eax,
                ebx_tmp = out(reg) ebx,
                out("ecx") ecx,
                out("edx") edx,
            );

            let offset = (i * 16) as usize;
            brand[offset..offset + 4].copy_from_slice(&eax.to_le_bytes());
            brand[offset + 4..offset + 8].copy_from_slice(&ebx.to_le_bytes());
            brand[offset + 8..offset + 12].copy_from_slice(&ecx.to_le_bytes());
            brand[offset + 12..offset + 16].copy_from_slice(&edx.to_le_bytes());
        }
    }

    brand
}

fn cpu_info() -> CpuInfo {
    let architecture = {
        #[cfg(target_arch = "x86_64")]
        {
            CpuArchitecture::X86_64
        }

        #[cfg(target_arch = "aarch64")]
        {
            CpuArchitecture::AArch64
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            CpuArchitecture::Unknown
        }
    };

    CpuInfo {
        vendor: get_cpu_vendor(),
        brand: get_cpu_brand(),
        architecture,
    }
}

fn setup_framebuffer(bs: &mut EfiBootServices) -> Option<FramebufferInfo> {
    let mut gop_ptr: *mut c_void = ptr::null_mut();
    let status = (bs.locate_protocol)(&GOP_GUID, ptr::null_mut(), &mut gop_ptr);

    if status != EFI_SUCCESS || gop_ptr.is_null() {
        return None;
    }

    let gop = unsafe { &*(gop_ptr as *const EfiGraphicsOutputProtocol) };

    if gop.mode.is_null() {
        return None;
    }

    let mode = unsafe { &*gop.mode };

    if mode.info.is_null() {
        return None;
    }

    let mode_info = unsafe { &*mode.info };

    let pixel_format = match mode_info.pixel_format {
        0 => PixelFormat::Rgb,
        1 => PixelFormat::Bgr,
        2 => PixelFormat::Bitmask,
        3 => PixelFormat::BltOnly,
        _ => PixelFormat::Unknown,
    };

    if pixel_format == PixelFormat::BltOnly {
        return None;
    }

    Some(FramebufferInfo {
        address: mode.frame_buffer_base,
        size: mode.frame_buffer_size,
        width: mode_info.horizontal_resolution,
        height: mode_info.vertical_resolution,
        pixels_per_scan_line: mode_info.pixels_per_scan_line,
        pixel_format,
        pixel_bitmask: mode_info.pixel_information,
    })
}

fn disable_watchdog(bs: &mut EfiBootServices) {
    let _ = (bs.set_watchdog_timer)(0, 0, 0, ptr::null_mut());
}

fn cleanup_pool(bs: &EfiBootServices, buf: &mut *mut c_void) {
    if !(*buf).is_null() {
        let _ = (bs.free_pool)(*buf);
        *buf = ptr::null_mut();
    }
}

#[no_mangle]
pub extern "win64" fn efi_main(image: EfiHandle, system_table: *mut c_void) -> EfiStatus {
    if system_table.is_null() {
        return EFI_INVALID_PARAMETER;
    }

    let st = unsafe { &mut *(system_table as *mut EfiSystemTable) };
    let bs = match unsafe { st.boot_services.as_mut() } {
        Some(bs) => bs,
        None => return EFI_INVALID_PARAMETER,
    };

    disable_watchdog(bs);

    let framebuffer_info = setup_framebuffer(bs);

    let mut mmap_buf: *mut c_void = ptr::null_mut();
    let mut mmap_buf_size: usize = 0;

    loop {
        let mut needed_size: usize = 0;
        let mut _map_key: usize = 0;
        let mut desc_size: usize = 0;
        let mut _desc_ver: u32 = 0;

        let status = (bs.get_memory_map)(
            &mut needed_size,
            ptr::null_mut(),
            &mut _map_key,
            &mut desc_size,
            &mut _desc_ver,
        );

        if status != EFI_BUFFER_TOO_SMALL && status != EFI_SUCCESS {
            cleanup_pool(bs, &mut mmap_buf);
            return status;
        }

        let slack = if desc_size != 0 { desc_size * 16 } else { 4096 };
        let alloc_size = needed_size.saturating_add(slack);

        if mmap_buf.is_null() || mmap_buf_size < alloc_size {
            cleanup_pool(bs, &mut mmap_buf);

            mmap_buf_size = alloc_size;
            let mut new_buf: *mut c_void = ptr::null_mut();
            let st_alloc = (bs.allocate_pool)(EFI_LOADER_DATA, mmap_buf_size, &mut new_buf);

            if st_alloc != EFI_SUCCESS || new_buf.is_null() {
                cleanup_pool(bs, &mut mmap_buf);
                return if st_alloc == EFI_SUCCESS {
                    EFI_INVALID_PARAMETER
                } else {
                    st_alloc
                };
            }

            mmap_buf = new_buf;
        }

        let mut actual_size = mmap_buf_size;
        let mut map_key2: usize = 0;
        let mut desc_size2: usize = 0;
        let mut _desc_ver2: u32 = 0;

        let st_map = (bs.get_memory_map)(
            &mut actual_size,
            mmap_buf as *mut EfiMemoryDescriptor,
            &mut map_key2,
            &mut desc_size2,
            &mut _desc_ver2,
        );

        if st_map == EFI_BUFFER_TOO_SMALL {
            mmap_buf_size = actual_size.saturating_add(desc_size2.saturating_mul(16));
            continue;
        }

        if st_map != EFI_SUCCESS {
            cleanup_pool(bs, &mut mmap_buf);
            return st_map;
        }

        let st_exit = (bs.exit_boot_services)(image, map_key2);

        if st_exit == EFI_INVALID_PARAMETER {
            continue;
        }

        if st_exit != EFI_SUCCESS {
            cleanup_pool(bs, &mut mmap_buf);
            return st_exit;
        }

        let boot_info = BOOT_INFO_STORAGE.call_once(|| BootInfo {
            memory_map: MemoryMap::new(mmap_buf as *const u8, actual_size, desc_size2),
            framebuffer: framebuffer_info.unwrap_or_else(FramebufferInfo::empty),
            framebuffer_present: framebuffer_info.is_some(),
            verbose: false,
            boot_method: BootMethod::Uefi,
            cpu: cpu_info(),
            init_payload: ExecutableImage::empty(),
        });

        unsafe {
            kmain(boot_info);
        }
    }
}
