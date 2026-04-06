//! Atom ABI — single source of truth for kernel ↔ userspace constants.
//!
//! Every constant that must agree across the kernel / userspace boundary
//! lives here.  Both the kernel crate and the `atom_syscall` userspace
//! library depend on `atom_abi`, making divergence structurally impossible.
//!
//! # Layout
//! - Error codes (`EINVAL`, `ENOSYS`, …)
//! - Error detection threshold and helper
//! - User virtual-address limits
//! - Syscall numbers

#![no_std]

use core::marker::PhantomData;

// ---------------------------------------------------------------------------
// Userspace virtual-address ABI types
// ---------------------------------------------------------------------------

/// Global ABI version for the kernel/userspace contract.
pub const ABI_VERSION: u32 = 1;

/// Canonical 64-bit userspace virtual address passed across the kernel/userspace ABI.
pub type UserVirtAddr = u64;

/// Typed userspace virtual address validated by the ABI contract.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[must_use]
pub struct UserVAddr(usize);

impl UserVAddr {
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    #[inline]
    pub const fn as_u64(self) -> UserVirtAddr {
        self.0 as UserVirtAddr
    }

    #[inline]
    pub const fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    #[inline]
    pub const fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }

    #[inline]
    pub fn checked_add(self, offset: usize) -> Result<Self, UserAddressError> {
        let next = self.0.checked_add(offset).ok_or(UserAddressError::Overflow)?;
        validate_user_addr(next)
    }
}

impl TryFrom<usize> for UserVAddr {
    type Error = UserAddressError;

    #[inline]
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        validate_user_addr(value)
    }
}

impl TryFrom<UserVirtAddr> for UserVAddr {
    type Error = UserAddressError;

    #[inline]
    fn try_from(value: UserVirtAddr) -> Result<Self, Self::Error> {
        validate_user_addr(value as usize)
    }
}

/// Typed userspace pointer validated by the ABI contract.
///
/// Guarantees:
/// - Canonical userspace virtual address.
/// - Inside [`USER_SPACE_MIN`, `USER_SPACE_MAX`).
///
/// Does NOT guarantee:
/// - Present mapping.
/// - Access permissions.
/// - Residency (may still page-fault).
#[repr(transparent)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[must_use]
pub struct UserPtr<T> {
    addr: UserVAddr,
    _marker: PhantomData<*const T>,
}

impl<T> Copy for UserPtr<T> {}

impl<T> Clone for UserPtr<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> UserPtr<T> {
    #[inline]
    const fn as_usize(self) -> usize {
        self.addr.as_usize()
    }

    #[inline]
    pub const fn as_ptr(self) -> *const T {
        self.addr.as_ptr::<T>()
    }

    #[inline]
    pub const fn as_mut_ptr(self) -> *mut T {
        self.addr.as_mut_ptr::<T>()
    }

    #[inline]
    pub fn checked_add_bytes(self, offset: usize) -> Result<Self, UserAddressError> {
        Ok(Self {
            addr: self.addr.checked_add(offset)?,
            _marker: PhantomData,
        })
    }

    /// Validate a byte range starting at this pointer.
    #[inline]
    pub fn byte_range(self, len: usize) -> Result<UserRange, UserAddressError> {
        validate_user_range(self.as_usize(), len)
    }

    #[inline]
    pub const fn cast<U>(self) -> UserPtr<U> {
        UserPtr {
            addr: self.addr,
            _marker: PhantomData,
        }
    }
}

impl<T> TryFrom<usize> for UserPtr<T> {
    type Error = UserAddressError;

    #[inline]
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        validate_user_ptr(value)
    }
}

impl<T> TryFrom<UserVirtAddr> for UserPtr<T> {
    type Error = UserAddressError;

    #[inline]
    fn try_from(value: UserVirtAddr) -> Result<Self, Self::Error> {
        validate_user_ptr(value as usize)
    }
}

/// Typed userspace range validated by the ABI contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct UserRange {
    base: UserVAddr,
    len: usize,
}

impl UserRange {
    #[inline]
    pub const fn base(self) -> UserVAddr {
        self.base
    }

    #[inline]
    pub const fn len(self) -> usize {
        self.len
    }

    #[inline]
    pub const fn start(self) -> usize {
        self.base.as_usize()
    }

    #[inline]
    pub fn end_exclusive(self) -> usize {
        self.base.as_usize() + self.len
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAddressError {
    NonCanonical,
    BelowUserMin,
    AboveUserMax,
    Overflow,
    Unaligned,
    EmptyRange,
}

/// Offset within a userspace-owned object or mapping.
///
/// Keep offsets distinct from absolute virtual addresses so APIs can make
/// intent explicit and avoid accidental base/offset recomposition bugs.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserOffset(pub u64);

// ---------------------------------------------------------------------------
// User virtual-address limits
// ---------------------------------------------------------------------------

/// Last valid byte in the lower-half canonical range on x86-64.
/// Any user-space pointer must be ≤ this value.
pub const USER_CANONICAL_MAX: UserVirtAddr = 0x0000_7FFF_FFFF_FFFF;

/// Alias for `USER_CANONICAL_MAX`.  Syscall return values strictly above
/// this are guaranteed to be kernel error codes (which live near `u64::MAX`).
pub const USER_VA_LIMIT: UserVirtAddr = USER_CANONICAL_MAX;

/// Lowest non-null userspace virtual address accepted across the ABI.
///
/// The first page remains guarded so that null-pointer dereferences fault
/// predictably instead of aliasing valid memory.
pub const USER_SPACE_MIN: UserVirtAddr = 0x0000_0000_0000_1000;

/// Exclusive upper bound for lower-half userspace pointers.
pub const USER_SPACE_MAX: UserVirtAddr = 0x0000_8000_0000_0000;

/// Bitmask used to flag suspiciously high bits in a userspace pointer.
pub const USER_PTR_HIGH_BITS_MASK: UserVirtAddr = 0xFFFF_0000_0000_0000;

/// ABI page size/alignment for userspace virtual-memory ranges.
pub const USER_PAGE_SIZE: usize = 4096;

// ---------------------------------------------------------------------------
// Syscall error codes
// ---------------------------------------------------------------------------

/// Threshold for detecting syscall error codes in raw return values.
///
/// Kernel error codes are defined as `u64::MAX - N` for small N.  Any raw
/// syscall return value **at or above** this threshold is an error code,
/// never a valid user-space virtual address.
///
/// The margin (256 slots) is generous enough to accommodate future error
/// codes without changing user-space detection logic.
pub const SYSCALL_ERROR_THRESHOLD: u64 = u64::MAX - 256;

pub const ESUCCESS: u64 = 0;
pub const EINVAL: u64 = u64::MAX - 1;
pub const ENOSYS: u64 = u64::MAX - 2;
pub const ENOMEM: u64 = u64::MAX - 3;
pub const EPERM: u64 = u64::MAX - 4;
pub const EBUSY: u64 = u64::MAX - 5;
pub const EMSGSIZE: u64 = u64::MAX - 6;
pub const ETIMEDOUT: u64 = u64::MAX - 7;
pub const EWOULDBLOCK: u64 = u64::MAX - 8;
pub const EDEADLK: u64 = u64::MAX - 9;
pub const ENOTFOUND: u64 = u64::MAX - 10;

// Filesystem error codes
pub const ENOENT: u64 = u64::MAX - 11;
pub const EEXIST: u64 = u64::MAX - 12;
pub const EISDIR: u64 = u64::MAX - 13;
pub const ENOTDIR: u64 = u64::MAX - 14;
pub const ENOTEMPTY: u64 = u64::MAX - 15;
pub const EBADF: u64 = u64::MAX - 16;
pub const EFBIG: u64 = u64::MAX - 17;
pub const ENOSPC: u64 = u64::MAX - 18;
pub const EROFS: u64 = u64::MAX - 19;
pub const ENAMETOOLONG: u64 = u64::MAX - 20;
pub const EIO: u64 = u64::MAX - 21;
pub const EACCES: u64 = u64::MAX - 22;
pub const EMFILE: u64 = u64::MAX - 23;
pub const EOVERFLOW: u64 = u64::MAX - 24;
pub const ECORRUPTED: u64 = u64::MAX - 25;
pub const EMLINK: u64 = u64::MAX - 26;
pub const EXDEV: u64 = u64::MAX - 27;
pub const EPIPE: u64 = u64::MAX - 28;
pub const ENOTSUP: u64 = u64::MAX - 29;
pub const EAGAIN: u64 = u64::MAX - 30;
pub const EINTR: u64 = u64::MAX - 31;
pub const E2BIG: u64 = u64::MAX - 32;
pub const ENODEV: u64 = u64::MAX - 33;  // No such device

// ---------------------------------------------------------------------------
// Infrastructure / Networking Phase 1 syscalls
// ---------------------------------------------------------------------------
pub const SYS_PCI_GET_BAR:            u64 = 81;
pub const SYS_DEVICE_BIND_IRQ:        u64 = 82;
pub const SYS_IRQ_LISTEN:             u64 = 83;
pub const SYS_DMA_ALLOC:              u64 = 84;
pub const SYS_DMA_MAP:                u64 = 85;
pub const SYS_DMA_FREE:               u64 = 86;
pub const SYS_MAP_MMIO:               u64 = 87;
pub const SYS_IRQ_ACK:                u64 = 88;
pub const SYS_PCI_QUERY_DEVICE:       u64 = 89;
pub const SYS_PCI_ENUMERATE:          u64 = 90;
pub const SYS_PCI_REQUEST_DEVICE:     u64 = 91;

/// Check whether a raw syscall return value represents an error code.
///
/// This is the **single authoritative way** to distinguish error returns
/// from valid addresses or counts.  Use this instead of ad-hoc
/// `>= u64::MAX - N` comparisons.
#[inline]
pub fn is_syscall_error(value: u64) -> bool {
    value >= SYSCALL_ERROR_THRESHOLD
}

/// Returns true when `addr` is canonical on x86-64.
#[inline]
pub fn is_canonical(addr: UserVirtAddr) -> bool {
    let sign = (addr >> 47) & 1;
    let upper = addr >> 48;

    match sign {
        0 => upper == 0,
        _ => upper == 0xFFFF,
    }
}

/// Returns true when `addr` is within the canonical lower userspace range.
#[inline]
pub fn is_canonical_user_va(addr: UserVirtAddr) -> bool {
    is_canonical(addr) && addr <= USER_CANONICAL_MAX
}

/// Returns true when `addr` is a non-null userspace address accepted by the ABI.
#[inline]
pub fn is_valid_user_va(addr: UserVirtAddr) -> bool {
    addr >= USER_SPACE_MIN && addr < USER_SPACE_MAX
}

/// Returns true when `addr` lies inside the anonymous mmap window.
#[inline]
pub fn is_user_mmap_addr(addr: UserVirtAddr) -> bool {
    addr >= USER_MMAP_START && addr < USER_MMAP_END
}

/// Returns true when the upper bits of `addr` look suspicious for a lower-half
/// userspace pointer.
#[inline]
pub fn has_suspicious_user_high_bits(addr: UserVirtAddr) -> bool {
    addr & USER_PTR_HIGH_BITS_MASK != 0
}

/// Validates and returns a typed userspace virtual address.
#[inline]
pub fn validate_user_addr(addr: usize) -> Result<UserVAddr, UserAddressError> {
    let addr_u64 = addr as UserVirtAddr;

    if !is_canonical(addr_u64) {
        return Err(UserAddressError::NonCanonical);
    }
    if addr_u64 < USER_SPACE_MIN {
        return Err(UserAddressError::BelowUserMin);
    }
    if addr_u64 >= USER_SPACE_MAX {
        return Err(UserAddressError::AboveUserMax);
    }

    Ok(UserVAddr(addr))
}

/// Validates and returns a typed userspace pointer.
#[inline]
pub fn validate_user_ptr<T>(ptr: usize) -> Result<UserPtr<T>, UserAddressError> {
    Ok(UserPtr {
        addr: validate_user_addr(ptr)?,
        _marker: PhantomData,
    })
}

/// Validates and returns a typed userspace virtual address range.
#[inline]
pub fn validate_user_range(base: usize, len: usize) -> Result<UserRange, UserAddressError> {
    if len == 0 {
        return Err(UserAddressError::EmptyRange);
    }

    let base = validate_user_addr(base)?;
    let end_exclusive = base
        .as_usize()
        .checked_add(len)
        .ok_or(UserAddressError::Overflow)?;

    if (end_exclusive as UserVirtAddr) > USER_SPACE_MAX {
        return Err(UserAddressError::AboveUserMax);
    }

    Ok(UserRange { base, len })
}

/// Validates a userspace range that must be page-aligned.
#[inline]
pub fn validate_user_page_aligned_range(base: usize, len: usize) -> Result<UserRange, UserAddressError> {
    let range = validate_user_range(base, len)?;
    if !range.start().is_multiple_of(USER_PAGE_SIZE) || !range.len().is_multiple_of(USER_PAGE_SIZE) {
        return Err(UserAddressError::Unaligned);
    }
    Ok(range)
}

/// Validates a userspace return address produced by a syscall path.
#[inline]
pub fn validate_user_return_addr(addr: usize) -> Result<UserVAddr, UserAddressError> {
    validate_user_addr(addr)
}

/// Validates a userspace return range produced by a syscall path.
#[inline]
pub fn validate_user_return_range(base: usize, len: usize) -> Result<UserRange, UserAddressError> {
    validate_user_range(base, len)
}

/// Validates a userspace mmap hint address.
///
/// Hints are not required to be page-aligned, but must still be valid
/// userspace addresses and lie in the mmap window.
#[inline]
pub fn validate_user_mmap_hint(addr: usize) -> Result<UserVAddr, UserAddressError> {
    let addr = validate_user_addr(addr)?;
    let raw = addr.as_u64();

    if raw < USER_MMAP_START {
        return Err(UserAddressError::BelowUserMin);
    }
    if raw >= USER_MMAP_END {
        return Err(UserAddressError::AboveUserMax);
    }

    Ok(addr)
}

/// Validates a userspace mmap range.
///
/// The returned range is guaranteed to:
/// - satisfy the canonical userspace ABI contract;
/// - be page-aligned; and
/// - lie fully inside the mmap window `[USER_MMAP_START, USER_MMAP_END)`.
#[inline]
pub fn validate_user_mmap_range(base: usize, len: usize) -> Result<UserRange, UserAddressError> {
    let range = validate_user_page_aligned_range(base, len)?;

    if range.start() < USER_MMAP_START as usize {
        return Err(UserAddressError::BelowUserMin);
    }

    if (range.end_exclusive() as UserVirtAddr) > USER_MMAP_END {
        return Err(UserAddressError::AboveUserMax);
    }

    Ok(range)
}

// ---------------------------------------------------------------------------
// Virtual memory syscall constants (mmap/munmap/mprotect/brk)
// ---------------------------------------------------------------------------

/// mmap protection flags
pub const PROT_NONE: u64 = 0;
pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;

/// mmap flags
pub const MAP_ANONYMOUS: u64 = 0x20;
pub const MAP_PRIVATE: u64 = 0x02;
pub const SYS_CAP_CHECK: u64 = 9;
pub const MAP_FIXED: u64 = 0x10;

/// Default user heap start (above typical text/data/bss)
pub const USER_HEAP_START: UserVirtAddr = 0x0000_0010_0000_0000;

/// Default mmap region for dynamic allocations
pub const USER_MMAP_START: UserVirtAddr = 0x0000_2000_0000_0000;
pub const USER_MMAP_END: UserVirtAddr = 0x0000_7000_0000_0000;

/// Standard userspace stack top.
pub const USER_STACK_TOP: UserVirtAddr = 0x0000_8000_0000;

// ---------------------------------------------------------------------------
// Infrastructure syscall structures (Phase 1 Networking)
// ---------------------------------------------------------------------------

/// Information about a PCI Base Address Register (BAR).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PciBarInfo {
    pub base_addr: u64,
    pub size: u64,
    pub mmio_cap: u64,
    pub is_mmio: u32,
    pub is_64bit: u32,
    pub prefetchable: u32,
}

/// Identity information about a PCI device, returned by SYS_PCI_QUERY_DEVICE.
/// Allows userspace drivers to inspect the device they hold a DeviceCap for
/// and decide whether they support it — without the kernel knowing anything
/// about specific drivers.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PciDeviceInfo {
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub irq_line: u8,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub _pad: u8,
}

/// Information returned when binding a device IRQ.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IrqBindInfo {
    pub irq_cap: u64,
    pub vector: u32,
    pub reserved: u32,
}

/// Parameters for DMA memory allocation.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DmaAllocParams {
    pub size: u64,
    pub align: u64,
    pub flags: u64,
}

/// Information about a DMA memory mapping.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DmaMappingInfo {
    pub user_va: u64,
    pub phys_addr: u64,
    pub size: u64,
}

/// Number of unmapped guard pages reserved below each userspace stack.
pub const USER_STACK_GUARD_PAGES: usize = 1;

/// Default number of mapped pages for each userspace stack.
pub const DEFAULT_USER_STACK_PAGES: usize = 128; // 512 KiB

/// Default mapped userspace stack size in bytes.
pub const DEFAULT_USER_STACK_SIZE: usize = DEFAULT_USER_STACK_PAGES * 4096;

// ---------------------------------------------------------------------------
// Graphics / video mode constants
// ---------------------------------------------------------------------------

/// Maximum number of video modes the BGA driver supports and the kernel
/// will report via SYS_GET_VIDEO_MODES.  Both the kernel (bga.rs) and
/// userspace callers (e.g. display_settings) must size their mode buffers
/// to at least this value; keeping the constant here prevents silent drift.
pub const VIDEO_MAX_MODES: usize = 16;

// ---------------------------------------------------------------------------
// IPC Port constants
// ---------------------------------------------------------------------------

/// Well-known ports for system services:
/// Port 1 = NAME_SERVICE (namesvc)
/// Port 2 = SERVICE_MANAGER (service_manager)
/// Port 3 = FS_SERVICE (fsd - Filesystem Daemon)
pub const PORT_FS_SERVICE: u64 = 3;
pub const PORT_BLOCK_SERVICE: u64 = 4;

// ---------------------------------------------------------------------------
// Filesystem limits
// ---------------------------------------------------------------------------

pub const FS_MAX_PATH_LEN: usize = 4096;
pub const FS_MAX_NAME_LEN: usize = 256;
pub const FS_MAX_FDS: usize = 1024;

// ---------------------------------------------------------------------------
// File open flags (O_* constants)
// ---------------------------------------------------------------------------

pub const O_RDONLY: u32 = 0x0000;
pub const O_WRONLY: u32 = 0x0001;
pub const O_RDWR: u32 = 0x0002;
pub const O_CREAT: u32 = 0x0040;
pub const O_EXCL: u32 = 0x0080;
pub const O_TRUNC: u32 = 0x0200;
pub const O_APPEND: u32 = 0x0400;
pub const O_NONBLOCK: u32 = 0x0800;
pub const O_SYNC: u32 = 0x1000;
pub const O_DIRECTORY: u32 = 0x10000;
pub const O_NOFOLLOW: u32 = 0x20000;
pub const O_CLOEXEC: u32 = 0x80000;

// ---------------------------------------------------------------------------
// Seek constants
// ---------------------------------------------------------------------------

pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

// ---------------------------------------------------------------------------
// File type masks (S_* constants for stat)
// ---------------------------------------------------------------------------

pub const S_IFMT: u32 = 0o170000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFSOCK: u32 = 0o140000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_lower_half_addresses_are_accepted() {
        assert!(is_canonical(0x0000_0000_2000_0000));
        assert!(is_canonical(USER_MMAP_START));
        assert!(is_canonical(USER_CANONICAL_MAX));
    }

    #[test]
    fn non_canonical_addresses_are_rejected() {
        assert!(!is_canonical(0x0000_8000_0000_0000));
        assert!(!is_canonical(0x0001_0000_0000_0000));
        assert!(!is_canonical(0x0000_2000_0000_0000 << 16));
    }

    #[test]
    fn user_va_validation_enforces_range_and_canonicality() {
        assert!(!is_valid_user_va(0));
        assert!(!is_valid_user_va(USER_SPACE_MIN - 1));
        assert!(is_valid_user_va(USER_SPACE_MIN));
        assert!(is_valid_user_va(USER_MMAP_START));
        assert!(!is_valid_user_va(0x0000_2000_0000_0000 << 16));
    }

    #[test]
    fn typed_user_addr_validation_enforces_abi_contract() {
        assert!(validate_user_addr(USER_SPACE_MIN as usize).is_ok());
        assert!(validate_user_addr((USER_SPACE_MAX - 1) as usize).is_ok());
        assert_eq!(
            validate_user_addr((USER_SPACE_MIN - 1) as usize),
            Err(UserAddressError::BelowUserMin)
        );
        assert_eq!(
            validate_user_addr(USER_SPACE_MAX as usize),
            Err(UserAddressError::AboveUserMax)
        );
        assert_eq!(
            validate_user_addr(0x0001_0000_0000_0000usize),
            Err(UserAddressError::NonCanonical)
        );
    }

    #[test]
    fn typed_user_ptr_validation_enforces_abi_contract() {
        assert!(validate_user_ptr::<u8>(USER_SPACE_MIN as usize).is_ok());
        assert_eq!(
            validate_user_ptr::<u8>((USER_SPACE_MIN - 1) as usize),
            Err(UserAddressError::BelowUserMin)
        );
        assert_eq!(
            validate_user_ptr::<u8>(USER_SPACE_MAX as usize),
            Err(UserAddressError::AboveUserMax)
        );
    }

    #[test]
    fn typed_user_range_validation_rejects_empty_and_overflowing_ranges() {
        assert_eq!(
            validate_user_range(USER_SPACE_MIN as usize, 0),
            Err(UserAddressError::EmptyRange)
        );
        assert!(validate_user_range(USER_SPACE_MIN as usize, 0x2000).is_ok());
        assert_eq!(
            validate_user_range((USER_SPACE_MAX - 1) as usize, 2),
            Err(UserAddressError::AboveUserMax)
        );
        assert_eq!(
            validate_user_range((USER_SPACE_MIN - 1) as usize, 8),
            Err(UserAddressError::BelowUserMin)
        );
        assert_eq!(
            validate_user_range(USER_SPACE_MIN as usize, usize::MAX),
            Err(UserAddressError::Overflow)
        );
    }

    #[test]
    fn user_return_addr_validation_reuses_abi_contract() {
        assert!(validate_user_return_addr(USER_SPACE_MIN as usize).is_ok());
        assert_eq!(
            validate_user_return_addr(USER_SPACE_MAX as usize),
            Err(UserAddressError::AboveUserMax)
        );
        assert_eq!(
            validate_user_return_addr(0x0001_0000_0000_0000usize),
            Err(UserAddressError::NonCanonical)
        );
    }

    #[test]
    fn user_return_range_validation_reuses_abi_contract() {
        assert!(validate_user_return_range(USER_SPACE_MIN as usize, USER_PAGE_SIZE).is_ok());
        assert_eq!(
            validate_user_return_range((USER_SPACE_MAX - USER_PAGE_SIZE as u64 / 2) as usize, USER_PAGE_SIZE),
            Err(UserAddressError::AboveUserMax)
        );
    }

    #[test]
    fn user_mmap_range_validation_enforces_window_alignment_and_overflow() {
        assert!(validate_user_mmap_range(USER_MMAP_START as usize, USER_PAGE_SIZE).is_ok());

        assert_eq!(
            validate_user_mmap_range((USER_MMAP_START - USER_PAGE_SIZE as u64) as usize, USER_PAGE_SIZE),
            Err(UserAddressError::BelowUserMin)
        );
        assert_eq!(
            validate_user_mmap_range(USER_MMAP_END as usize, USER_PAGE_SIZE),
            Err(UserAddressError::AboveUserMax)
        );
        assert_eq!(
            validate_user_mmap_range((USER_MMAP_END - USER_PAGE_SIZE as u64 / 2) as usize, USER_PAGE_SIZE),
            Err(UserAddressError::Unaligned)
        );
        assert_eq!(
            validate_user_mmap_range((USER_MMAP_END - USER_PAGE_SIZE as u64) as usize, USER_PAGE_SIZE * 2),
            Err(UserAddressError::AboveUserMax)
        );
        assert_eq!(
            validate_user_mmap_range((USER_MMAP_START + 1) as usize, USER_PAGE_SIZE),
            Err(UserAddressError::Unaligned)
        );
        assert_eq!(
            validate_user_mmap_range(USER_MMAP_START as usize, USER_PAGE_SIZE + 1),
            Err(UserAddressError::Unaligned)
        );
    }

    #[test]
    fn user_page_aligned_range_validation_enforces_alignment() {
        assert!(validate_user_page_aligned_range(USER_SPACE_MIN as usize, USER_PAGE_SIZE).is_ok());
        assert_eq!(
            validate_user_page_aligned_range((USER_SPACE_MIN + 1) as usize, USER_PAGE_SIZE),
            Err(UserAddressError::Unaligned)
        );
        assert_eq!(
            validate_user_page_aligned_range(USER_SPACE_MIN as usize, USER_PAGE_SIZE + 1),
            Err(UserAddressError::Unaligned)
        );
    }

    #[test]
    fn user_mmap_hint_validation_accepts_unaligned_hints_inside_window() {
        assert!(validate_user_mmap_hint(USER_MMAP_START as usize).is_ok());
        assert!(validate_user_mmap_hint((USER_MMAP_START + 1) as usize).is_ok());
        assert_eq!(
            validate_user_mmap_hint((USER_MMAP_START - 1) as usize),
            Err(UserAddressError::BelowUserMin)
        );
        assert_eq!(
            validate_user_mmap_hint(USER_MMAP_END as usize),
            Err(UserAddressError::AboveUserMax)
        );
    }
}
