// Memory Management Subsystem
//
// Serves as the top-level entry point for all kernel memory management
// components. This module coordinates initialization of physical memory,
// virtual memory, heap allocation, and address space management.
//
// Key responsibilities:
// - Initialize all memory management layers in the correct dependency order
// - Provide a single, clear initialization interface for early kernel boot
// - Encapsulate MM submodules behind a unified namespace
//
// Initialization flow:
// - `pmm::init` sets up the physical memory manager using the UEFI memory map
// - `vm::init` establishes kernel virtual memory mappings and paging structures
// - `heap::init` initializes the global kernel heap allocator
// - `addrspace::init` prepares user address space management facilities
//
// Design principles:
// - Strict layering: each subsystem builds on the previous one
// - Explicit ordering to avoid subtle early-boot memory hazards
// - Minimal logic in this module; responsibilities are delegated downward
//
// Correctness and safety notes:
// - The UEFI memory map is the authoritative source for usable physical memory
// - Virtual memory must be initialized before any heap or user mappings
// - Heap initialization assumes paging and kernel mappings are already active
// - Address space management relies on both PMM and VM being fully operational
//
// Intended usage:
// - Called once during kernel boot, before spawning threads or enabling
//   user-space execution

pub mod pmm;
pub mod heap;
pub mod vm;
pub mod addrspace;
pub mod policy;
#[allow(dead_code)]
pub mod vma;
#[allow(dead_code)]
pub mod oom;

use crate::boot::MemoryMap;
use core::fmt;

// Constants for validation
const PAGE_SIZE: usize = 4096;
const KERNEL_BASE: usize = 0xFFFF_8000_0000_0000;

/// Validation errors for memory operations
///
/// This enum represents all possible validation failures that can occur
/// during memory management operations. Each variant includes relevant
/// context to aid in debugging and error reporting.
///
/// Design rationale:
/// - Provides specific error variants for different validation failures
/// - Includes context (addresses, limits, resource IDs) for debugging
/// - Supports comprehensive error propagation without panicking
/// - Integrates with the memory safety validation layer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    /// Address is not properly aligned to required boundary
    Unaligned {
        addr: usize,
        required_alignment: usize,
    },
    /// Address is outside valid bounds
    OutOfBounds {
        addr: usize,
        min: usize,
        max: usize,
    },
    /// Attempted operation on a protected resource
    ProtectedResource {
        resource_id: u64,
    },
    /// Operation attempted before subsystem initialization
    NotInitialized,
    /// Size parameter is invalid
    InvalidSize {
        size: usize,
        max_size: usize,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::Unaligned { addr, required_alignment } => {
                write!(
                    f,
                    "Address 0x{:x} is not aligned to {} bytes",
                    addr, required_alignment
                )
            }
            ValidationError::OutOfBounds { addr, min, max } => {
                write!(
                    f,
                    "Address 0x{:x} is out of bounds (valid range: 0x{:x} - 0x{:x})",
                    addr, min, max
                )
            }
            ValidationError::ProtectedResource { resource_id } => {
                write!(
                    f,
                    "Cannot operate on protected resource (ID: 0x{:x})",
                    resource_id
                )
            }
            ValidationError::NotInitialized => {
                write!(f, "Memory subsystem not yet initialized")
            }
            ValidationError::InvalidSize { size, max_size } => {
                write!(
                    f,
                    "Invalid size {} (maximum allowed: {})",
                    size, max_size
                )
            }
        }
    }
}

/// Validates that an address is page-aligned (4096 bytes)
///
/// This function checks if the provided address is aligned to the page boundary.
/// Page alignment is required for all memory mapping operations to ensure proper
/// hardware page table functionality.
///
/// # Arguments
/// * `addr` - The address to validate
///
/// # Returns
/// * `Ok(())` if the address is page-aligned
/// * `Err(ValidationError::Unaligned)` if the address is not page-aligned
///
/// # Examples
/// ```
/// // Valid page-aligned address
/// assert!(validate_page_alignment(0x1000).is_ok());
/// 
/// // Invalid unaligned address
/// assert!(validate_page_alignment(0x1001).is_err());
/// ```
pub fn validate_page_alignment(addr: usize) -> Result<(), ValidationError> {
    if !addr.is_multiple_of(PAGE_SIZE) {
        return Err(ValidationError::Unaligned {
            addr,
            required_alignment: PAGE_SIZE,
        });
    }
    Ok(())
}

/// Validates that a range of addresses is page-aligned
///
/// This function checks if both the start and end addresses of a range are
/// page-aligned. This is required for operations that work with memory ranges
/// such as mapping multiple pages or validating memory regions.
///
/// # Arguments
/// * `start` - The start address of the range
/// * `end` - The end address of the range
///
/// # Returns
/// * `Ok(())` if both addresses are page-aligned
/// * `Err(ValidationError::Unaligned)` if either address is not page-aligned
///
/// # Examples
/// ```
/// // Valid page-aligned range
/// assert!(validate_page_range(0x1000, 0x2000).is_ok());
/// 
/// // Invalid range with unaligned start
/// assert!(validate_page_range(0x1001, 0x2000).is_err());
/// 
/// // Invalid range with unaligned end
/// assert!(validate_page_range(0x1000, 0x2001).is_err());
/// ```
pub fn validate_page_range(start: usize, end: usize) -> Result<(), ValidationError> {
    validate_page_alignment(start)?;
    validate_page_alignment(end)?;
    Ok(())
}

pub fn validate_initialized(initialized: bool) -> Result<(), ValidationError> {
    if initialized {
        Ok(())
    } else {
        Err(ValidationError::NotInitialized)
    }
}

pub fn validate_size(size: usize, max_size: usize) -> Result<(), ValidationError> {
    if size == 0 || size > max_size {
        Err(ValidationError::InvalidSize { size, max_size })
    } else {
        Ok(())
    }
}

pub fn validate_unprotected_resource(
    resource_id: u64,
    is_protected: bool,
) -> Result<(), ValidationError> {
    if is_protected {
        Err(ValidationError::ProtectedResource { resource_id })
    } else {
        Ok(())
    }
}

/// Validates that a user-space address and size are within valid bounds
///
/// This function ensures that user-space memory operations stay within the
/// lower canonical address range and do not overlap with kernel space. The
/// kernel space begins at KERNEL_BASE (0xFFFF_8000_0000_0000), and all
/// user-space addresses must be strictly below this boundary.
///
/// # Arguments
/// * `addr` - The starting address to validate
/// * `size` - The size of the memory region in bytes
///
/// # Returns
/// * `Ok(())` if the entire range [addr, addr+size) is in valid user space
/// * `Err(ValidationError::OutOfBounds)` if any part of the range is invalid
///
/// # Examples
/// ```
/// // Valid user-space address
/// assert!(validate_user_space_bounds(0x1000, 0x1000).is_ok());
/// 
/// // Invalid kernel-space address
/// assert!(validate_user_space_bounds(0xFFFF_8000_0000_0000, 0x1000).is_err());
/// 
/// // Invalid range that crosses into kernel space
/// assert!(validate_user_space_bounds(0xFFFF_7FFF_FFFF_F000, 0x2000).is_err());
/// ```
pub fn validate_user_space_bounds(addr: usize, size: usize) -> Result<(), ValidationError> {
    // Check if the starting address is in kernel space
    if addr >= KERNEL_BASE {
        return Err(ValidationError::OutOfBounds {
            addr,
            min: 0,
            max: KERNEL_BASE - 1,
        });
    }
    
    // Check for overflow when computing end address
    let end = addr.checked_add(size).ok_or(ValidationError::OutOfBounds {
        addr,
        min: 0,
        max: KERNEL_BASE - 1,
    })?;
    
    // Check if the end address crosses into kernel space
    if end > KERNEL_BASE {
        return Err(ValidationError::OutOfBounds {
            addr,
            min: 0,
            max: KERNEL_BASE - 1,
        });
    }
    
    Ok(())
}

pub unsafe fn init(memory_map: &MemoryMap) {
    pmm::init(memory_map);
    vm::init(memory_map);
    heap::init();
    addrspace::init();
    policy::init();
    vma::init();
    oom::init();
}

#[cfg(test)]
mod tests {
    use super::{
        validate_page_alignment, validate_size, validate_unprotected_resource,
        validate_user_space_bounds, ValidationError,
    };

    #[test]
    fn protected_resource_validation_rejects_protected_ids() {
        assert_eq!(
            validate_unprotected_resource(0x1234, true),
            Err(ValidationError::ProtectedResource { resource_id: 0x1234 })
        );
    }

    #[test]
    fn invalid_size_validation_rejects_zero_and_oversized_values() {
        assert_eq!(
            validate_size(0, 16),
            Err(ValidationError::InvalidSize {
                size: 0,
                max_size: 16,
            })
        );
        assert_eq!(
            validate_size(32, 16),
            Err(ValidationError::InvalidSize {
                size: 32,
                max_size: 16,
            })
        );
    }

    #[test]
    fn alignment_validation_rejects_unaligned_addresses() {
        assert_eq!(
            validate_page_alignment(0x1001),
            Err(ValidationError::Unaligned {
                addr: 0x1001,
                required_alignment: 4096,
            })
        );
    }

    #[test]
    fn user_space_bounds_validation_rejects_kernel_and_overflow_ranges() {
        assert!(matches!(
            validate_user_space_bounds(0xFFFF_8000_0000_0000, 0x1000),
            Err(ValidationError::OutOfBounds { .. })
        ));
        assert!(matches!(
            validate_user_space_bounds(usize::MAX - 0x1000, 0x2000),
            Err(ValidationError::OutOfBounds { .. })
        ));
    }
}
