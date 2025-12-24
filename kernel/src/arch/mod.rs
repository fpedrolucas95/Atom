// Architecture Abstraction Layer
//
// Provides low-level, architecture-specific primitives used by the kernel.
// This module exposes a minimal and explicit interface to CPU instructions
// that cannot be expressed safely or portably in pure Rust.
//
// Key responsibilities:
// - Offer a unified API for halting the CPU across architectures
// - Read critical processor registers for debugging and kernel logic
// - Abstract stack pointer access (RSP/SP) per architecture
// - Expose descriptor table state (GDT/IDT/TR) for introspection
//
// Design principles:
// - Architecture-specific code is isolated behind `cfg(target_arch)` gates
// - All functions are small, `#[inline(always)]`, and zero-overhead
// - Unsafe inline assembly is tightly scoped and well-defined
// - Unsupported architectures degrade gracefully with safe fallbacks
//
// Implementation details:
// - Uses `hlt` (x86_64) and `wfi` (aarch64) for low-power CPU halt
// - Reads registers like RSP, RFLAGS, CR3, and TR directly via assembly
// - Retrieves GDT and IDT descriptors using `sgdt` and `sidt`
// - Packed descriptor structs match the CPU-defined memory layout
//
// Correctness and safety notes:
// - All inline assembly preserves flags and avoids memory side effects
// - Functions returning zero on unsupported architectures are marked
//   with `#[allow(unreachable_code)]` to satisfy the compiler
// - Intended primarily for kernel initialization, diagnostics, and debugging

#[inline(always)]
pub fn halt() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
    }

    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    loop {
        core::hint::spin_loop();
    }
}

#[inline(always)]
pub fn current_rsp() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
        rsp
    }
}

#[inline(always)]
#[allow(dead_code)]
pub fn rflags() -> u64 {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    unsafe {
        let flags: u64;
        core::arch::asm!("pushfq; pop {}", out(reg) flags, options(nomem, preserves_flags));
        return flags;
    }

    #[allow(unreachable_code)]
    0
}

#[inline(always)]
pub fn read_cr3() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, preserves_flags));
        return cr3;
    }

    #[allow(unreachable_code)]
    0
}

#[inline(always)]
#[allow(dead_code)]
pub fn read_tr() -> u16 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let tr: u16;
        core::arch::asm!("str ax", out("ax") tr, options(nomem, preserves_flags));
        tr
    }
}

#[inline(always)]
#[allow(dead_code)]
pub fn read_gdt() -> (u16, u64) {
    #[repr(C, packed)]
    struct Descriptor {
        limit: u16,
        base: u64,
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        let mut desc = Descriptor { limit: 0, base: 0 };
        core::arch::asm!("sgdt [{}]", in(reg) &mut desc, options(nostack, preserves_flags));
        (desc.limit, desc.base)
    }
}

#[inline(always)]
#[allow(dead_code)]
pub fn read_idt() -> (u16, u64) {
    #[repr(C, packed)]
    struct Descriptor {
        limit: u16,
        base: u64,
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        let mut desc = Descriptor { limit: 0, base: 0 };
        core::arch::asm!("sidt [{}]", in(reg) &mut desc, options(nostack, preserves_flags));
        (desc.limit, desc.base)
    }
}

#[inline(always)]
#[allow(dead_code)]
pub fn current_privilege_level() -> u8 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let cs: u16;
        core::arch::asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags));
        (cs & 0x3) as u8
    }
}

pub mod gdt;
