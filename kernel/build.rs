// kernel/build.rs
//
// Single source of truth for IRQ vector assignments.
//
// Changing a vector number here propagates automatically to both consumers:
//   - Rust  : kernel/target/.../out/vectors.rs  (included by interrupts/mod.rs)
//   - NASM  : kernel/src/interrupts/vectors.inc  (included by handlers.asm)
//
// Do NOT edit vectors.inc or the generated vectors.rs by hand; edit this
// file instead and rebuild.
//
// Architectural constraint — IDT vector range partitioning on x86_64:
//
//   The Intel/AMD x86_64 architecture permanently reserves IDT vectors
//   0x00–0x1F (0–31) for processor-defined exceptions:
//
//     0x00  #DE  Divide Error
//     0x01  #DB  Debug Exception
//     0x02  —    Non-Maskable Interrupt
//     0x03  #BP  Breakpoint
//     0x04  #OF  Overflow
//     0x05  #BR  BOUND Range Exceeded
//     0x06  #UD  Invalid Opcode
//     0x07  #NM  Device Not Available
//     0x08  #DF  Double Fault
//     0x09  —    Coprocessor Segment Overrun (reserved/obsolete)
//     0x0A  #TS  Invalid TSS
//     0x0B  #NP  Segment Not Present
//     0x0C  #SS  Stack-Segment Fault
//     0x0D  #GP  General Protection Fault
//     0x0E  #PF  Page Fault
//     0x0F  —    Reserved
//     0x10  #MF  x87 Floating-Point Exception
//     0x11  #AC  Alignment Check
//     0x12  #MC  Machine Check
//     0x13  #XM  SIMD Floating-Point Exception
//     0x14  #VE  Virtualization Exception
//     0x15  #CP  Control Protection Exception
//     0x16–0x1F  —  Reserved
//
//   Assigning a hardware IRQ (PIC/APIC) to any of these vectors produces
//   silent, catastrophic aliasing: a timer tick, for example, would be
//   decoded by the CPU as a General Protection Fault, triggering the wrong
//   handler with an incorrect error code.  The resulting behaviour is
//   non-deterministic, extremely difficult to reproduce, and effectively
//   undebuggable under production conditions.
//
//   All entries in the `vectors` table below that represent hardware
//   interrupts (i.e., routed via PIC or APIC) MUST therefore carry a
//   value ≥ 0x20.  This invariant is enforced at build time immediately
//   below the table definition.
//
//   Reference: Intel® 64 and IA-32 Architectures Software Developer's
//              Manual, Volume 3A, Section 6.3 "Sources of Interrupts and
//              Exceptions", Table 6-1 "Protected-Mode Exceptions and
//              Interrupts".

use std::{env, fs, path::PathBuf, process::Command};

/// Minimum legal IDT vector for hardware interrupts on x86_64.
///
/// The architecture permanently reserves vectors 0x00–0x1F for CPU-defined
/// exceptions.  Any PIC/APIC-routed interrupt must use a vector ≥ this value.
/// See the module-level comment above and Intel SDM Vol. 3A §6.3, Table 6-1.
const FIRST_SAFE_IRQ_VECTOR: u8 = 0x20;

fn main() {
    // -----------------------------------------------------------------------
    // Vector table — edit here to change assignments.
    // Values must fit in a u8 (0–255).
    // -----------------------------------------------------------------------
    let vectors: &[(&str, u8)] = &[
        ("TIMER_INTERRUPT_VECTOR",      32),
        ("KEYBOARD_INTERRUPT_VECTOR",   33),
        ("MOUSE_INTERRUPT_VECTOR",      44),
        ("RESCHEDULE_INTERRUPT_VECTOR", 45),
        ("USER_TRAP_INTERRUPT_VECTOR",  0x68),
    ];

    // -----------------------------------------------------------------------
    // Build-time architectural guard.
    //
    // Reject any vector assignment that falls inside the CPU-reserved
    // exception range.  This check runs unconditionally on every `cargo
    // build`, so an invalid edit is caught immediately with an actionable
    // error message rather than surviving to produce a silently broken
    // kernel image.
    //
    // Implementation note: `panic!` in a build script causes `cargo` to
    // print the message and abort the build with a non-zero exit code,
    // which is exactly the desired behaviour.
    // -----------------------------------------------------------------------
    for &(name, val) in vectors {
        if val < FIRST_SAFE_IRQ_VECTOR {
            panic!(
                "\n\n\
                 *** INVALID INTERRUPT VECTOR ASSIGNMENT ***\n\
                 \n\
                 Constant : {name}\n\
                 Value    : {val:#04X} ({val})\n\
                 Required : >= {FIRST_SAFE_IRQ_VECTOR:#04X} ({FIRST_SAFE_IRQ_VECTOR})\n\
                 \n\
                 x86_64 permanently reserves IDT vectors 0x00–0x1F for CPU-defined\n\
                 exceptions (#DE, #DB, #NMI, #BP, … #CP).  Routing a hardware IRQ\n\
                 (PIC/APIC) into this range aliases it with a CPU exception handler,\n\
                 producing non-deterministic faults that are effectively undebuggable.\n\
                 \n\
                 Fix : assign a value >= 0x20 (32) to `{name}` in kernel/build.rs.\n\
                 Ref : Intel SDM Vol. 3A §6.3, Table 6-1.\n"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Rust side — write OUT_DIR/vectors.rs, included by interrupts/mod.rs.
    // -----------------------------------------------------------------------
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut rs = String::from("// Auto-generated by kernel/build.rs — do not edit manually.\n");
    for (name, val) in vectors {
        rs.push_str(&format!("pub const {}: u8 = {};\n", name, val));
    }
    fs::write(out_dir.join("vectors.rs"), &rs).unwrap();

    // -----------------------------------------------------------------------
    // NASM side — write kernel/src/interrupts/vectors.inc, included by
    // handlers.asm via `%include "vectors.inc"`.  Written into the same
    // directory as handlers.asm so NASM can find it without an extra -I flag.
    // -----------------------------------------------------------------------
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let inc_path = manifest_dir
        .join("src")
        .join("interrupts")
        .join("vectors.inc");
    let mut inc =
        String::from("; Auto-generated by kernel/build.rs — do not edit manually.\n");
    for (name, val) in vectors {
        inc.push_str(&format!("%define {:<28} {}\n", name, val));
    }
    fs::write(&inc_path, &inc).unwrap();

    // -----------------------------------------------------------------------
    // AP trampoline (flat binary) used for INIT/SIPI startup.
    // -----------------------------------------------------------------------
    let trampoline_src = manifest_dir.join("src").join("ap_trampoline.asm");
    let trampoline_out = out_dir.join("ap_trampoline.bin");

    let status = Command::new("nasm")
        .arg("-f")
        .arg("bin")
        .arg(&trampoline_src)
        .arg("-o")
        .arg(&trampoline_out)
        .status()
        .expect("failed to execute nasm for AP trampoline");

    if !status.success() {
        panic!(
            "nasm failed while assembling AP trampoline: {}",
            trampoline_src.display()
        );
    }

    println!("cargo:rerun-if-changed={}", trampoline_src.display());


    println!("cargo:rerun-if-changed=build.rs");
}
