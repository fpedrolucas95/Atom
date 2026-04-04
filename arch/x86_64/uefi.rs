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
    PixelFormat, EfiMemoryDescriptor, EfiPixelBitmask, DriverList, DriverImage, MAX_DRIVER_NAME_LEN,
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
    handle_protocol: EfiHandleProtocol,
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

// Simple File System Protocol GUID
const SIMPLE_FILE_SYSTEM_GUID: EfiGuid = EfiGuid {
    data1: 0x0964E5B22,
    data2: 0x6459,
    data3: 0x11D2,
    data4: [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
};

// Loaded Image Protocol GUID
const LOADED_IMAGE_GUID: EfiGuid = EfiGuid {
    data1: 0x5B1B31A1,
    data2: 0x9562,
    data3: 0x11D2,
    data4: [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
};

// File modes
const EFI_FILE_MODE_READ: u64 = 0x0000000000000001;

// EFI File Protocol
type EfiFileOpen = extern "win64" fn(
    this: *mut EfiFileProtocol,
    new_handle: *mut *mut EfiFileProtocol,
    file_name: *const u16,
    open_mode: u64,
    attributes: u64,
) -> EfiStatus;

type EfiFileClose = extern "win64" fn(this: *mut EfiFileProtocol) -> EfiStatus;

type EfiFileRead = extern "win64" fn(
    this: *mut EfiFileProtocol,
    buffer_size: *mut usize,
    buffer: *mut c_void,
) -> EfiStatus;

type EfiFileGetInfo = extern "win64" fn(
    this: *mut EfiFileProtocol,
    information_type: *const EfiGuid,
    buffer_size: *mut usize,
    buffer: *mut c_void,
) -> EfiStatus;

#[repr(C)]
struct EfiFileProtocol {
    revision: u64,
    open: EfiFileOpen,
    close: EfiFileClose,
    _delete: usize,
    read: EfiFileRead,
    _write: usize,
    _get_position: usize,
    _set_position: usize,
    get_info: EfiFileGetInfo,
    // ... more fields
}

// Simple File System Protocol
type EfiOpenVolume = extern "win64" fn(
    this: *mut EfiSimpleFileSystemProtocol,
    root: *mut *mut EfiFileProtocol,
) -> EfiStatus;

#[repr(C)]
struct EfiSimpleFileSystemProtocol {
    revision: u64,
    open_volume: EfiOpenVolume,
}

// Loaded Image Protocol
#[repr(C)]
struct EfiLoadedImageProtocol {
    revision: u32,
    parent_handle: EfiHandle,
    system_table: *mut c_void,
    device_handle: EfiHandle,
    // ... more fields we don't need
}

// File Info GUID
const EFI_FILE_INFO_GUID: EfiGuid = EfiGuid {
    data1: 0x09576E92,
    data2: 0x6D3F,
    data3: 0x11D2,
    data4: [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
};

// File attribute for directory
const EFI_FILE_DIRECTORY: u64 = 0x0000000000000010;

// Handle Protocol function type
type EfiHandleProtocol = extern "win64" fn(
    handle: EfiHandle,
    protocol: *const EfiGuid,
    interface: *mut *mut c_void,
) -> EfiStatus;

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

/// Convert ASCII string to UTF-16 for UEFI
fn str_to_utf16(s: &str, buf: &mut [u16]) -> usize {
    let mut i = 0;
    for c in s.chars() {
        if i >= buf.len() - 1 {
            break;
        }
        buf[i] = c as u16;
        i += 1;
    }
    buf[i] = 0; // null terminator
    i + 1
}

/// Load init.atxf from the boot volume
fn load_init_payload(
    image: EfiHandle,
    bs: &mut EfiBootServices,
) -> Option<ExecutableImage> {
    // Get Loaded Image Protocol to find our device handle
    let mut loaded_image_ptr: *mut c_void = ptr::null_mut();
    let status = (bs.handle_protocol)(
        image,
        &LOADED_IMAGE_GUID,
        &mut loaded_image_ptr,
    );

    if status != EFI_SUCCESS || loaded_image_ptr.is_null() {
        return None;
    }

    let loaded_image = unsafe { &*(loaded_image_ptr as *const EfiLoadedImageProtocol) };
    let device_handle = loaded_image.device_handle;

    if device_handle.is_null() {
        return None;
    }

    // Get Simple File System Protocol from device handle
    let mut fs_ptr: *mut c_void = ptr::null_mut();
    let status = (bs.handle_protocol)(
        device_handle,
        &SIMPLE_FILE_SYSTEM_GUID,
        &mut fs_ptr,
    );

    if status != EFI_SUCCESS || fs_ptr.is_null() {
        return None;
    }

    let fs = unsafe { &mut *(fs_ptr as *mut EfiSimpleFileSystemProtocol) };

    // Open root volume
    let mut root: *mut EfiFileProtocol = ptr::null_mut();
    let status = (fs.open_volume)(fs as *mut _, &mut root);

    if status != EFI_SUCCESS || root.is_null() {
        return None;
    }

    // Try multiple paths for init.atxf (the init process, PID 1)
    let paths = [
        "\\EFI\\BOOT\\init.atxf",
        "\\init.atxf",
        "\\drivers\\init.atxf",
    ];

    let mut path_buf = [0u16; 64];
    let mut file: *mut EfiFileProtocol = ptr::null_mut();
    let root_ref = unsafe { &mut *root };

    for path in paths.iter() {
        str_to_utf16(path, &mut path_buf);
        let status = (root_ref.open)(
            root,
            &mut file,
            path_buf.as_ptr(),
            EFI_FILE_MODE_READ,
            0,
        );

        if status == EFI_SUCCESS && !file.is_null() {
            break;
        }
        file = ptr::null_mut();
    }

    if file.is_null() {
        let _ = (root_ref.close)(root);
        return None;
    }

    let file_ref = unsafe { &mut *file };

    // Get file size via GetInfo
    let mut info_buf = [0u8; 256];
    let mut info_size: usize = 256;
    let status = (file_ref.get_info)(
        file,
        &EFI_FILE_INFO_GUID,
        &mut info_size,
        info_buf.as_mut_ptr() as *mut c_void,
    );

    if status != EFI_SUCCESS {
        let _ = (file_ref.close)(file);
        let _ = (root_ref.close)(root);
        return None;
    }

    // File size is at offset 8 in EFI_FILE_INFO
    let file_size = unsafe {
        *(info_buf.as_ptr().add(8) as *const u64) as usize
    };

    if file_size == 0 || file_size > 16 * 1024 * 1024 {
        // Sanity check: max 16 MB
        let _ = (file_ref.close)(file);
        let _ = (root_ref.close)(root);
        return None;
    }

    // Allocate buffer for file
    let mut file_buffer: *mut c_void = ptr::null_mut();
    let status = (bs.allocate_pool)(EFI_LOADER_DATA, file_size, &mut file_buffer);

    if status != EFI_SUCCESS || file_buffer.is_null() {
        let _ = (file_ref.close)(file);
        let _ = (root_ref.close)(root);
        return None;
    }

    // Read file contents
    let mut read_size = file_size;
    let status = (file_ref.read)(file, &mut read_size, file_buffer);

    // Close handles
    let _ = (file_ref.close)(file);
    let _ = (root_ref.close)(root);

    if status != EFI_SUCCESS || read_size != file_size {
        let _ = (bs.free_pool)(file_buffer);
        return None;
    }

    Some(ExecutableImage {
        ptr: file_buffer as *const u8,
        size: file_size,
    })
}

fn cleanup_pool(bs: &EfiBootServices, buf: &mut *mut c_void) {
    if !(*buf).is_null() {
        let _ = (bs.free_pool)(*buf);
        *buf = ptr::null_mut();
    }
}

/// Load a single ATXF file from a given directory on the EFI volume.
///
/// `dir_prefix` is the directory path WITH trailing backslash,
/// e.g. `"\\system\\services\\"` or `"\\apps\\user\\"`.
fn load_driver_file(
    root: *mut EfiFileProtocol,
    dir_prefix: &str,
    filename: &[u16],
    bs: &mut EfiBootServices,
) -> Option<(ExecutableImage, [u8; MAX_DRIVER_NAME_LEN])> {
    // Build full path: <dir_prefix><filename>
    let mut path_buf = [0u16; 128];
    let mut idx = 0;
    for c in dir_prefix.chars() {
        if idx >= path_buf.len() - 1 {
            // dir_prefix alone fills the buffer — path would be truncated; skip.
            return None;
        }
        path_buf[idx] = c as u16;
        idx += 1;
    }
    for &c in filename.iter() {
        if c == 0 { break; }
        if idx >= path_buf.len() - 1 {
            // Combined path is too long to fit — skip rather than silently truncate.
            return None;
        }
        path_buf[idx] = c;
        idx += 1;
    }
    path_buf[idx] = 0;

    // Open the file
    let mut file: *mut EfiFileProtocol = ptr::null_mut();
    let root_ref = unsafe { &mut *root };
    let status = (root_ref.open)(
        root,
        &mut file,
        path_buf.as_ptr(),
        EFI_FILE_MODE_READ,
        0,
    );

    if status != EFI_SUCCESS || file.is_null() {
        return None;
    }

    let file_ref = unsafe { &mut *file };

    // Get file size via GetInfo
    let mut info_buf = [0u8; 256];
    let mut info_size: usize = 256;
    let status = (file_ref.get_info)(
        file,
        &EFI_FILE_INFO_GUID,
        &mut info_size,
        info_buf.as_mut_ptr() as *mut c_void,
    );

    if status != EFI_SUCCESS {
        let _ = (file_ref.close)(file);
        return None;
    }

    // File size is at offset 8 in EFI_FILE_INFO
    let file_size = unsafe {
        *(info_buf.as_ptr().add(8) as *const u64) as usize
    };

    if file_size == 0 || file_size > 16 * 1024 * 1024 {
        let _ = (file_ref.close)(file);
        return None;
    }

    // Allocate buffer for file
    let mut file_buffer: *mut c_void = ptr::null_mut();
    let status = (bs.allocate_pool)(EFI_LOADER_DATA, file_size, &mut file_buffer);

    if status != EFI_SUCCESS || file_buffer.is_null() {
        let _ = (file_ref.close)(file);
        return None;
    }

    // Read file contents
    let mut read_size = file_size;
    let status = (file_ref.read)(file, &mut read_size, file_buffer);

    let _ = (file_ref.close)(file);

    if status != EFI_SUCCESS || read_size != file_size {
        let _ = (bs.free_pool)(file_buffer);
        return None;
    }

    // Extract driver name from filename (remove .atxf extension)
    let mut name = [0u8; MAX_DRIVER_NAME_LEN];
    for (idx, &c) in filename.iter().enumerate() {
        if c == 0 || c == b'.' as u16 { break; }
        if idx >= MAX_DRIVER_NAME_LEN - 1 { break; }
        name[idx] = c as u8;
    }

    Some((
        ExecutableImage {
            ptr: file_buffer as *const u8,
            size: file_size,
        },
        name,
    ))
}

/// Scan a single directory on the already-opened root volume for .atxf files
/// and add each one to `driver_list`.
///
/// `dir_path`   — directory path WITHOUT trailing backslash (e.g. `"\\system\\services"`)
/// `dir_prefix` — directory path WITH    trailing backslash (e.g. `"\\system\\services\\"`)
///
/// Missing directories are silently skipped so that a partial disk layout
/// does not abort the boot sequence.
fn load_atxf_from_dir(
    root: *mut EfiFileProtocol,
    dir_path: &str,
    dir_prefix: &str,
    bs: &mut EfiBootServices,
    driver_list: &mut DriverList,
) {
    let mut path_buf = [0u16; 64];
    // Guard: str_to_utf16 silently truncates; bail out if dir_path doesn't fit
    // (need room for all chars plus the null terminator).
    if dir_path.len() >= path_buf.len() {
        return;
    }
    str_to_utf16(dir_path, &mut path_buf);

    let mut dir_handle: *mut EfiFileProtocol = ptr::null_mut();
    let root_ref = unsafe { &mut *root };
    let status = (root_ref.open)(
        root,
        &mut dir_handle,
        path_buf.as_ptr(),
        EFI_FILE_MODE_READ,
        0,
    );

    if status != EFI_SUCCESS || dir_handle.is_null() {
        // Directory absent — skip silently
        return;
    }

    let dir_ref = unsafe { &mut *dir_handle };
    let mut entry_buf = [0u8; 512];

    loop {
        let mut buf_size: usize = 512;
        let status = (dir_ref.read)(
            dir_handle,
            &mut buf_size,
            entry_buf.as_mut_ptr() as *mut c_void,
        );

        if status != EFI_SUCCESS || buf_size == 0 {
            break;
        }

        // EFI_FILE_INFO layout:
        //   Offset  0: Size (u64)
        //   Offset  8: FileSize (u64)
        //   Offset 16: PhysicalSize (u64)
        //   Offset 24: CreateTime (EFI_TIME, 16 bytes)
        //   Offset 40: LastAccessTime (16 bytes)
        //   Offset 56: ModificationTime (16 bytes)
        //   Offset 72: Attribute (u64)
        //   Offset 80: FileName (CHAR16[])

        let attr = unsafe { *(entry_buf.as_ptr().add(72) as *const u64) };

        // Skip sub-directories
        if attr & EFI_FILE_DIRECTORY != 0 {
            continue;
        }

        // Read filename (UTF-16, null-terminated)
        let filename_ptr = unsafe { entry_buf.as_ptr().add(80) as *const u16 };
        let mut filename = [0u16; 64];
        for (i, slot) in filename.iter_mut().enumerate().take(63) {
            let c = unsafe { *filename_ptr.add(i) };
            *slot = c;
            if c == 0 { break; }
        }

        // Accept only .atxf files
        let mut len = 0usize;
        for (i, &c) in filename.iter().enumerate() {
            if c == 0 { len = i; break; }
        }
        let is_atxf = len > 5 && {
            let e = len - 5;
            filename[e]   == '.' as u16
            && (filename[e+1] == 'a' as u16 || filename[e+1] == 'A' as u16)
            && (filename[e+2] == 't' as u16 || filename[e+2] == 'T' as u16)
            && (filename[e+3] == 'x' as u16 || filename[e+3] == 'X' as u16)
            && (filename[e+4] == 'f' as u16 || filename[e+4] == 'F' as u16)
        };

        if !is_atxf {
            continue;
        }

        if driver_list.count >= crate::boot::MAX_BOOT_DRIVERS {
            break;
        }

        if let Some((img, name)) = load_driver_file(root, dir_prefix, &filename, bs) {
            driver_list.drivers[driver_list.count] = DriverImage { name, image: img };
            driver_list.count += 1;
        }
    }

    let _ = (dir_ref.close)(dir_handle);
}

/// OS partition directories scanned at boot for ATXF executables.
///
/// Each entry is `(dir_path, dir_prefix)`:
///   `dir_path`   — path WITHOUT trailing backslash, passed to EFI directory open
///   `dir_prefix` — path WITH    trailing backslash, prepended to each filename
///
/// This is the single source of truth for the disk layout.  Any change here
/// must be mirrored in `build.sh` / `build.ps1` (which place the .atxf files)
/// and `load_atxf_from_dir` / `load_driver_file` (which read them back).
const OS_ATXF_DIRS: &[(&str, &str)] = &[
    ("\\system\\services", "\\system\\services\\"),
    ("\\apps\\system",     "\\apps\\system\\"),
    ("\\apps\\user",       "\\apps\\user\\"),
];

/// Load all ATXF executables from the OS partition into the driver registry.
///
/// Scans every directory listed in `OS_ATXF_DIRS`.  Missing directories are
/// silently skipped so that a partial disk layout does not abort the boot
/// sequence.  The EFI partition (`\EFI\BOOT\`) is intentionally excluded:
/// `init.atxf` is loaded separately by `load_init_payload()` and is never
/// exposed to userspace.
fn load_drivers(
    image: EfiHandle,
    bs: &mut EfiBootServices,
) -> DriverList {
    let mut driver_list = DriverList::empty();

    // Obtain the root volume handle via the Loaded Image Protocol
    let mut loaded_image_ptr: *mut c_void = ptr::null_mut();
    let status = (bs.handle_protocol)(
        image,
        &LOADED_IMAGE_GUID,
        &mut loaded_image_ptr,
    );

    if status != EFI_SUCCESS || loaded_image_ptr.is_null() {
        return driver_list;
    }

    let loaded_image = unsafe { &*(loaded_image_ptr as *const EfiLoadedImageProtocol) };
    let device_handle = loaded_image.device_handle;

    if device_handle.is_null() {
        return driver_list;
    }

    let mut fs_ptr: *mut c_void = ptr::null_mut();
    let status = (bs.handle_protocol)(
        device_handle,
        &SIMPLE_FILE_SYSTEM_GUID,
        &mut fs_ptr,
    );

    if status != EFI_SUCCESS || fs_ptr.is_null() {
        return driver_list;
    }

    let fs = unsafe { &mut *(fs_ptr as *mut EfiSimpleFileSystemProtocol) };

    let mut root: *mut EfiFileProtocol = ptr::null_mut();
    let status = (fs.open_volume)(fs as *mut _, &mut root);

    if status != EFI_SUCCESS || root.is_null() {
        return driver_list;
    }

    // Scan every OS partition directory listed in OS_ATXF_DIRS.
    // Each scan is independent — a missing directory is silently skipped.
    for &(dir_path, dir_prefix) in OS_ATXF_DIRS {
        load_atxf_from_dir(root, dir_path, dir_prefix, bs, &mut driver_list);
    }

    let root_ref = unsafe { &mut *root };
    let _ = (root_ref.close)(root);

    driver_list
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

    // Load the init payload (init.atxf) from the boot volume
    let init_payload = load_init_payload(image, bs)
        .unwrap_or_else(ExecutableImage::empty);

    // Load drivers from \drivers\ directory at boot time
    // These are pre-loaded so they can be spawned by userspace via spawn_process()
    // Note: QEMU uses virtual FAT which doesn't work with AHCI, so we must
    // load drivers at boot when EFI filesystem protocol is still available
    let drivers = load_drivers(image, bs);

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
            init_payload,
            drivers,
        });

        unsafe {
            kmain(boot_info);
        }
    }
}
