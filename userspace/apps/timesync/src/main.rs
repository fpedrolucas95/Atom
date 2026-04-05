#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::debug::log;
use atom_syscall::thread::{exit, yield_now};

// ============================================================================
// Bump allocator — 64 KB heap
// ============================================================================

const HEAP_SIZE: usize = 64 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0u8; HEAP_SIZE]),
            next: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(16);
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let end = aligned + size;
            if end > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange_weak(cur, end, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! {
    loop {}
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    log("timesync: PANIC");
    loop {}
}

// ============================================================================
// Entry point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // 1. Wait for netd to be available
    log("timesync: waiting for netd...");
    let netd_port;
    let mut found = false;
    let mut port_val = 0u64;

    'outer: for _ in 0..200000u32 {
        match libipc::protocol::lookup_service("netd") {
            Ok(p) => {
                port_val = p;
                found = true;
                break 'outer;
            }
            Err(_) => {
                yield_now();
            }
        }
    }

    if !found {
        log("timesync: netd not found, giving up");
        exit(1);
    }

    netd_port = port_val;

    // 2. netd found
    log("timesync: netd found, fetching time...");

    // 3. HTTP GET
    match libnet::http_get(netd_port, "worldtimeapi.org", "/api/timezone/UTC", 80) {
        Err(_) => {
            // 4. Error
            log("timesync: HTTP GET failed");
            exit(1);
        }
        Ok(resp) => {
            // 5. Parse datetime from body
            let body = &resp.body;
            let needle = b"\"datetime\":\"";

            // Search for needle in body
            let mut found_pos: Option<usize> = None;
            if body.len() >= needle.len() {
                for i in 0..=(body.len() - needle.len()) {
                    if &body[i..i + needle.len()] == needle {
                        found_pos = Some(i + needle.len());
                        break;
                    }
                }
            }

            match found_pos {
                Some(start) => {
                    // Copy bytes until closing '"', max 32 bytes
                    let mut dt_buf = [0u8; 32];
                    let mut dt_len = 0usize;
                    for &b in &body[start..] {
                        if b == b'"' || dt_len >= 32 {
                            break;
                        }
                        dt_buf[dt_len] = b;
                        dt_len += 1;
                    }

                    // Build log message: "timesync: current UTC time: <datetime>"
                    let mut msg_buf = [0u8; 64];
                    let prefix = b"timesync: current UTC time: ";
                    let prefix_len = prefix.len();
                    msg_buf[..prefix_len].copy_from_slice(prefix);
                    let copy_len = dt_len.min(64 - prefix_len);
                    msg_buf[prefix_len..prefix_len + copy_len]
                        .copy_from_slice(&dt_buf[..copy_len]);

                    if let Ok(s) = core::str::from_utf8(&msg_buf[..prefix_len + copy_len]) {
                        log(s);
                    } else {
                        log("timesync: current UTC time: (invalid utf8)");
                    }
                }
                None => {
                    log("timesync: could not parse datetime from response");
                }
            }

            // 6. Exit cleanly
            exit(0);
        }
    }
}
