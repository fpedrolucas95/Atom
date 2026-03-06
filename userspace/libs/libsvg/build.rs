use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let libs_dir = manifest_dir
        .parent()
        .expect("libsvg parent missing");

    let libsvg_build = libs_dir.join("libsvg_c").join("build");
    let libxml2_build = libs_dir.join("libxml2").join("build");

    println!("cargo:rerun-if-changed={}", libs_dir.join("libsvg_c/Makefile").display());
    println!("cargo:rerun-if-changed={}", libs_dir.join("libxml2/Makefile").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("src/lib.rs").display());

    let libsvg_archive = libsvg_build.join("libsvg_atom.a");
    let libxml2_archive = libxml2_build.join("libxml2_atom.a");

    if libsvg_archive.exists() {
        println!("cargo:rustc-link-search=native={}", libsvg_build.display());
        println!("cargo:rustc-link-lib=static=svg_atom");
    } else {
        println!(
            "cargo:warning=libsvg_atom.a not found at {}; build it via build.sh or make -C userspace/libs/libsvg_c",
            libsvg_archive.display()
        );
    }

    if libxml2_archive.exists() {
        println!("cargo:rustc-link-search=native={}", libxml2_build.display());
        println!("cargo:rustc-link-lib=static=xml2_atom");
    } else {
        println!(
            "cargo:warning=libxml2_atom.a not found at {}; build it via build.sh or make -C userspace/libs/libxml2",
            libxml2_archive.display()
        );
    }
}