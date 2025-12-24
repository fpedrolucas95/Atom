// System Information Registry
//
// Provides a centralized, immutable view of system-wide boot and CPU
// information discovered during early kernel initialization. This module
// exposes read-only access to hardware and boot metadata for diagnostics
// and policy decisions.
//
// Key responsibilities:
// - Store CPU identification and architecture information
// - Record the system boot method (UEFI vs Legacy)
// - Provide safe, global access to this data after initialization
//
// Design principles:
// - Write-once, read-many: system information is initialized exactly once
// - Global immutability enforced via `spin::Once`
// - No dynamic allocation or mutation after boot
//
// Implementation details:
// - `SystemInfo` aggregates `CpuInfo` and `BootMethod` from the UEFI layer
// - `SYSTEM_INFO` uses `Once` to guarantee single initialization
// - Accessors return borrowed data or static string representations
// - Architecture and boot method are normalized into human-readable strings
//
// Correctness and safety notes:
// - Calling `info()` before `init()` is a hard error (panic)
// - Assumes CPU and boot detection is complete before subsystems query it
// - String trimming handles null-padded CPU brand identifiers safely
//
// Intended usage:
// - Boot banners and diagnostics
// - Conditional logic based on architecture or boot environment
// - Debug output and system introspection utilities
//
// This module acts as the kernel’s authoritative source of identity and
// environment information once bootstrapping is complete.
use crate::boot::{BootMethod, CpuArchitecture, CpuInfo};
use spin::Once;

#[allow(dead_code)]
pub struct SystemInfo {
    cpu: CpuInfo,
    boot: BootMethod,
}

static SYSTEM_INFO: Once<SystemInfo> = Once::new();

pub fn init(cpu: CpuInfo, boot: BootMethod) {
    SYSTEM_INFO.call_once(|| SystemInfo { cpu, boot });
}

#[allow(dead_code)]
pub fn info() -> &'static SystemInfo {
    SYSTEM_INFO.get().expect("SystemInfo not initialized")
}

impl SystemInfo {
    #[allow(dead_code)]
    pub fn cpu_brand(&self) -> &str {
        trim_nulls(&self.cpu.brand)
    }

    #[allow(dead_code)]
    pub fn architecture(&self) -> &'static str {
        match self.cpu.architecture {
            CpuArchitecture::X86_64 => "x86_64",
            CpuArchitecture::AArch64 => "aarch64",
            CpuArchitecture::Unknown => "unknown",
        }
    }

    #[allow(dead_code)]
    pub fn boot_method(&self) -> &'static str {
        match self.boot {
            BootMethod::Uefi => "UEFI",
            BootMethod::Legacy => "Legacy BIOS",
        }
    }
}

#[allow(dead_code)]
fn trim_nulls(bytes: &[u8]) -> &str {
    let last = bytes.iter().rposition(|&b| b != 0).map(|idx| idx + 1).unwrap_or(0);
    core::str::from_utf8(&bytes[..last]).unwrap_or("")
}