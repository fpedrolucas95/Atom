// Atom OS Userspace Syscall Library
//
// This library provides safe wrappers around system calls for use by userspace
// programs running in ring 3. All interaction with kernel services must go
// through these syscall interfaces.
//
// This library is designed to be:
// - Completely standalone (no kernel dependencies)
// - Minimal and efficient
// - Safe where possible, clearly marked unsafe where not

#![no_std]
#![allow(dead_code)]

pub mod raw;
pub mod thread;
pub mod input;
pub mod graphics;
pub mod io;
pub mod ipc;
pub mod debug;
pub mod error;

// Re-export common types at crate root
pub use error::{SyscallError, SyscallResult};
pub use raw::{syscall0, syscall1, syscall2, syscall3};
