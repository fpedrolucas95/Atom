// Thread management syscalls

use crate::raw::{syscall0, syscall1, syscall2, numbers::*};

/// Yield CPU to scheduler
/// 
/// Gives up the current timeslice and allows other threads to run.
/// This is a cooperative yielding mechanism.
#[inline]
pub fn yield_now() {
    unsafe {
        syscall0(SYS_THREAD_YIELD);
    }
}

/// Exit current thread with exit code
///
/// This function never returns. The thread is terminated and its
/// resources are cleaned up by the kernel.
#[inline]
pub fn exit(code: u64) -> ! {
    unsafe {
        syscall1(SYS_THREAD_EXIT, code);
    }
    // Safety: syscall should not return, but if it does, loop forever
    loop {
        core::hint::spin_loop();
    }
}

/// Sleep for a specified number of milliseconds
///
/// The thread will be suspended for at least the specified duration.
/// Actual sleep time may be longer due to scheduling.
#[inline]
pub fn sleep_ms(milliseconds: u64) {
    unsafe {
        syscall1(SYS_THREAD_SLEEP, milliseconds);
    }
}

/// Get system ticks (timer interrupts since boot)
///
/// Returns the number of timer ticks since the system booted.
/// Each tick is typically 10ms (100Hz timer).
#[inline]
pub fn get_ticks() -> u64 {
    unsafe {
        syscall0(SYS_GET_TICKS)
    }
}

/// Get approximate system time in milliseconds since boot
#[inline]
pub fn get_time_ms() -> u64 {
    get_ticks() * 10  // Assuming 100Hz timer (10ms per tick)
}


/// Return the logical CPU id currently executing this thread.
#[inline]
pub fn get_cpu_id() -> u64 {
    unsafe { syscall0(SYS_GET_CPU_ID) }
}

/// Return the number of online logical CPUs.
#[inline]
pub fn get_cpu_count() -> u64 {
    unsafe { syscall0(SYS_GET_CPU_COUNT) }
}

/// Set affinity mask for the current thread.
///
/// Bit N in `mask` allows execution on CPU N.
#[inline]
pub fn set_affinity(mask: u64) -> bool {
    unsafe { syscall2(SYS_SET_THREAD_AFFINITY, 0, mask) == 0 }
}

/// Query affinity mask for the current thread.
#[inline]
pub fn get_affinity() -> u64 {
    unsafe { syscall1(SYS_GET_THREAD_AFFINITY, 0) }
}

