// Simple Bump Allocator for Userspace Applications
//
// This allocator provides a simple bump pointer allocation strategy suitable
// for no_std environments where complex memory management isn't needed.
//
// Limitations:
// - No deallocation support (memory is never freed)
// - Fixed-size heap (1MB by default)
// - Not thread-safe without external synchronization
//
// This is suitable for simple userspace applications that don't need
// sophisticated memory management. For production use, consider a more
// advanced allocator like `linked_list_allocator`.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

/// A simple bump allocator with a fixed-size heap
pub struct BumpAllocator {
    heap: UnsafeCell<[u8; 1024 * 1024]>, // 1MB heap
    next: UnsafeCell<usize>,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    /// Create a new bump allocator
    pub const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0; 1024 * 1024]),
            next: UnsafeCell::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let next = self.next.get();
        let heap = self.heap.get();
        
        let align = layout.align();
        let size = layout.size();
        
        // Align the next pointer
        let offset = (*next + align - 1) & !(align - 1);
        let new_next = offset + size;
        
        if new_next > (*heap).len() {
            // Out of memory
            return core::ptr::null_mut();
        }
        
        *next = new_next;
        (*heap).as_mut_ptr().add(offset)
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator doesn't support deallocation
        // Memory is never freed
    }
}

/// Macro to define a global bump allocator
/// 
/// Usage:
/// ```
/// use atom_syscall::alloc::BumpAllocator;
/// atom_syscall::define_global_allocator!();
/// ```
#[macro_export]
macro_rules! define_global_allocator {
    () => {
        #[global_allocator]
        static ALLOCATOR: $crate::alloc::BumpAllocator = $crate::alloc::BumpAllocator::new();
    };
}
