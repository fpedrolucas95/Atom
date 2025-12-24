// Build Metadata and Versioning
//
// Defines compile-time build information for the kernel, including version,
// development phase, and human-readable banners. This data is embedded
// directly into the binary and used for diagnostics and boot-time reporting.
//
// Key responsibilities:
// - Centralize kernel identity (name and version)
// - Encode development phase and milestone information
// - Provide preformatted strings for logs and boot banners
//
// Design principles:
// - Compile-time constants only (no runtime overhead)
// - Single source of truth for versioning and build identity
// - Macro-based definition to avoid duplication and inconsistencies
//
// Implementation details:
// - `define_build_meta!` expands into multiple `pub const` string slices
// - Uses `concat!` to build derived strings at compile time
// - Exposes both verbose and short phase descriptors
// - Suppresses dead-code warnings for metadata not always referenced
//
// Usage notes:
// - `BOOT_BANNER` is intended for early boot output
// - `VERSION_TAG` is suitable for logs, panic messages, and diagnostics
// - Phase-related constants help correlate logs with kernel milestones
//
// Maintenance considerations:
// - Updating the kernel version or phase requires changing only one macro call
// - Build date is manually specified, making builds reproducible and explicit
// - Macro can be reused for future kernels or variant builds

macro_rules! define_build_meta {
    ($kernel_name:literal, $version:literal, $phase:literal, $phase_label:literal, $build_date:literal) => {
        #[allow(dead_code)]
        pub const KERNEL_NAME: &str = $kernel_name;
        #[allow(dead_code)]
        pub const VERSION: &str = $version;
        #[allow(dead_code)]
        pub const PHASE: &str = $phase;
        #[allow(dead_code)]
        pub const PHASE_LABEL: &str = $phase_label;
        #[allow(dead_code)]
        pub const BUILD_DATE: &str = $build_date;

        #[allow(dead_code)]
        pub const VERSION_TAG: &str = concat!($kernel_name, " v", $version);
        #[allow(dead_code)]
        pub const PHASE_ACTIVE: &str = concat!("Phase ", $phase, " - ", $phase_label, " Active");
        #[allow(dead_code)]
        pub const PHASE_SHORT: &str = concat!($phase, " (", $phase_label, " Active)");
        pub const BOOT_BANNER: &str = concat!(
            $kernel_name,
            " v",
            $version,
            " - Phase ",
            $phase,
            ": ",
            $phase_label
        );
    };
}

define_build_meta!(
    "Atom Kernel",
    "0.1.0",
    "6.3",
    "Service Manager & Declarative Boot",
    "2025-12-22"
);