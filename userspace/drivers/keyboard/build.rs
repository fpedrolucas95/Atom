/// Build script: resolve the userspace linker script via CARGO_MANIFEST_DIR
/// so the path is always absolute and works on Windows, Linux, and macOS
/// regardless of the working directory cargo is invoked from.
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let ld_script = std::path::PathBuf::from(&manifest_dir)
        .join("..")
        .join("..")
        .join("userspace.ld");

    let ld_script = std::fs::canonicalize(&ld_script).unwrap_or_else(|e| {
        panic!(
            "Cannot find userspace.ld at {}: {}",
            ld_script.display(),
            e
        )
    });

    // On Windows, canonicalize() returns UNC paths (\\?\C:\...) which some
    // tools do not understand.  Strip the prefix when present.
    let ld_str = ld_script.display().to_string();
    let ld_str = ld_str.strip_prefix(r"\\?\").unwrap_or(&ld_str);

    println!("cargo:rustc-link-arg=-T{}", ld_str);
    println!("cargo:rerun-if-changed={}", ld_str);
}
