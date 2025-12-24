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

use crate::boot::MemoryMap;

pub unsafe fn init(memory_map: &MemoryMap) {
    pmm::init(memory_map);
    vm::init(memory_map);
    heap::init();
    addrspace::init();
    policy::init();
}