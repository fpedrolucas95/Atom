# embed_binary

Binary embedding tool for Atom OS build system.

## Purpose

Converts binary files (like ATXF executables) into Rust source code as byte arrays that can be embedded in the kernel.

## Building

```bash
# Linux/macOS
cd tools/embed_binary
cargo build --release --target x86_64-unknown-linux-gnu

# Windows
cd tools\embed_binary
cargo build --release --target x86_64-pc-windows-msvc
```

The compiled binary will be at:
- Linux: `target/x86_64-unknown-linux-gnu/release/embed_binary`
- Windows: `target\x86_64-pc-windows-msvc\release\embed_binary.exe`

## Usage

```bash
embed_binary <input_binary> <output_rs_file> <const_name>
```

### Example

```bash
# Convert ui_shell.atxf to Rust source
./tools/embed_binary/target/x86_64-unknown-linux-gnu/release/embed_binary \
    kernel/ui_shell.atxf \
    kernel/src/ui_shell_binary.rs \
    UI_SHELL_BINARY
```

This generates:
```rust
// Auto-generated: Embedded ui_shell.atxf
// Size: 20480 bytes

pub const UI_SHELL_BINARY: &[u8] = &[
    0x41, 0x54, 0x58, 0x46, 0x01, 0x00, 0x24, 0x00,
    // ... more bytes ...
];
```

## Integration with Build Scripts

The tool is designed to be called from both `build.sh` (Linux/macOS) and `build.ps1` (Windows) during the kernel build process.

No Python dependency required!
