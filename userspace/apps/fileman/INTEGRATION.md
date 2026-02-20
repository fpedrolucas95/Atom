# Fileman Integration Guide

This document explains how to integrate fileman into the Atom OS boot sequence and filesystem.

## Project Structure

```
userspace/apps/fileman/
├── Cargo.toml                 # Rust package configuration
├── Makefile                   # Build automation
├── README.md                  # User documentation
├── BUILD.md                   # Build instructions
├── QUICKSTART.md              # Quick reference guide
├── ARCHITECTURE.md            # Technical architecture
├── INTEGRATION.md             # This file
└── src/
    ├── main.rs                # CLI interface & command dispatcher
    ├── error.rs               # Error handling & validation
    └── fs.rs                  # Filesystem abstractions
```

## File Descriptions

### Cargo.toml
**Purpose**: Rust package manifest  
**Dependencies**:
- `atom_syscall` - Kernel syscall wrappers
- `atom_abi` - ABI constants and types

**Profile Configuration**:
- Release: LTO enabled, aggressive optimization
- Result: ~100-150 KB binary

### src/main.rs
**Purpose**: Main entry point and CLI loop  
**Responsibilities**:
- Command parsing and dispatching
- Interactive shell interface
- User I/O and prompts
- Process lifecycle

**Key Types**:
- `CommandContext` - Maintains cwd and handles commands
- Command handlers for: pwd, cd, ls, cat, mkdir, rm, mv, cp, help

**Entry Point**: `extern "C" fn main() -> i32`

### src/error.rs
**Purpose**: Error handling and path validation  
**Exports**:
- `FilManagerError` enum with POSIX mappings
- `CommandError` for syntax errors
- Path validation & normalization functions
- User-friendly error messages

**Exit Codes**:
- 0: Success
- 1-39: POSIX error codes (see error.rs)

**Key Functions**:
- `normalize_path()` - Cleans and validates paths
- `validate_path_len()` - Ensures max 4096 bytes
- Error mappers to user messages

### src/fs.rs
**Purpose**: Filesystem abstraction layer  
**Key Types**:
- `File` - RAII file handle with read/write
- `Dir` - Directory listing with entry caching
- `DirEntry` - Directory entry with metadata
- `FsOps` - High-level operations (mkdir, copy, delete)
- `FsQuery` - File inspection (exists, is_dir, stat)

**Design Pattern**: Resource-safe RAII with Drop traits

## Building the Application

### Simple Build

```bash
cd userspace/apps/fileman
cargo build --release --target=x86_64-unknown-uefi
```

Output: `target/x86_64-unknown-uefi/release/fileman` (~150 KB)

### With Makefile

```bash
cd userspace/apps/fileman

# Build release version
make build

# Build and install
make install

# Test in QEMU
make test

# Show help
make help
```

### From Workspace Root

```bash
./build.sh  # Builds entire system including fileman
```

## Boot Integration

### Option 1: Launch from Init Process

Edit `/userspace/services/init/src/main.rs`:

```rust
// In Phase 2 (after UI shell starts)
log("[Phase 2] Spawning fileman...");
let _fileman_pid = match spawn_with_retry("fileman", 3) {
    Ok(pid) => {
        log_fmt!("[Phase 2] fileman spawned (PID {})", pid);
        pid
    },
    Err(_) => {
        log("[Phase 2] Failed to spawn fileman");
        0
    }
};
```

### Option 2: Launch from Terminal

User can manually launch fileman from terminal/shell:

```bash
$ fileman
fileman:/> help
```

### Option 3: Launch from Service Manager

Register fileman as a managed service:

```bash
# File: userspace/services/service_manager/fileman.service
[Service]
Type=simple
ExecStart=/efi/drivers/fileman
Restart=on-failure
RestartSec=5
```

## Installation & Packaging

### Include in EFI Image

The build process automatically includes fileman if it's in:
- `userspace/apps/fileman/` 
- And `./build.sh` includes the app

### Manual Integration Steps

```bash
# 1. Build fileman
cd userspace/apps/fileman
cargo build --release --target=x86_64-unknown-uefi

# 2. Copy to EFI drivers (if using EFI boot)
cp target/x86_64-unknown-uefi/release/fileman \
   ../../efi/drivers/fileman

# 3. Or use elf2atxf converter
../../tools/elf2atxf/target/release/elf2atxf \
   target/x86_64-unknown-uefi/release/fileman \
   -o ../../efi/drivers/fileman.atxf

# 4. Rebuild kernel/bootloader
cd ../..
./build.sh

# 5. Run in QEMU
cargo run --release
```

## Filesystem Assumptions

Fileman assumes:
- FAT32 filesystem (kernel default)
- Root directory mounted at `/`
- Standard directory structure:
  - `/home` - User home directories
  - `/etc` - Configuration files
  - `/tmp` - Temporary files
  - `/var` - Variable data

## API Usage (For Other Programs)

Other applications can use fileman's fs module:

```rust
// Add to Cargo.toml:
# [dependencies]
# fileman = { path = "../../apps/fileman" }

use fileman::fs::{File, Dir, FsOps, FileMode};

// Open and read file
let mut file = File::open("/etc/config.txt", FileMode::ReadOnly)?;
let contents = file.read_all()?;

// Copy file
FsOps::copy("/source.iso", "/backup/source.iso")?;

// Check if directory exists
let exists = FsQuery::exists("/home")?;

// List directory
let dir = Dir::open("/etc")?;
for entry in dir.entries() {
    println!("{}", entry.name);
}
```

## Testing & Validation

### Unit Tests

```bash
cd userspace/apps/fileman
cargo test --target=x86_64-unknown-uefi
```

### Integration Tests

```bash
# Build and run
./build.sh
cargo run --release

# In QEMU test commands
fileman:/> mkdir /test
fileman:/> cd /test
fileman:/test> cat /etc/hostname
fileman:/test> exit
```

### Manual Test Checklist

- [ ] Can list directories: `ls /home`
- [ ] Can change directories: `cd /etc`
- [ ] Can create directories: `mkdir /tmp/test`
- [ ] Can view files: `cat /etc/hostname`
- [ ] Can copy files: `cp /etc/hostname /tmp/copy`
- [ ] Can rename files: `mv /tmp/copy /tmp/backup`
- [ ] Can delete files: `rm /tmp/backup`
- [ ] Can handle errors gracefully
- [ ] Exits cleanly: `exit`

## Performance Tuning

### Optimize Binary Size

```bash
# In Cargo.toml [profile.release]
opt-level = "z"        # Minimize size
lto = true             # Link-time optimization
codegen-units = 1      # Better optimization

# Rebuild
cargo build --release --target=x86_64-unknown-uefi -Z build-std=core,alloc
```

### Memory Tuning

Current heap allocation: 1 MB

If running on memory-constrained system:
```rust
// In src/allocator.rs
const HEAP_SIZE: usize = 512 * 1024;  // Reduce to 512 KB if needed
```

### I/O Buffer Tuning

Current buffer sizes:
- Directory listing: 64 KB
- File copy: 64 KB
- File read: 4 KB (syscall layer)

For embedded systems with limited RAM:
```rust
// In src/fs.rs
const BUF_SIZE: usize = 16384;  // Reduce from 65536
```

## Troubleshooting Integration

### Build Fails: Missing Dependencies

```bash
# Ensure workspace dependencies are built first
cd shared/abi
cargo build

cd userspace/libs/syscall
cargo build

# Then build fileman
cd ../../apps/fileman
cargo build --release --target=x86_64-unknown-uefi
```

### Build Fails: Linker Errors

```bash
# Clean and rebuild all projects
cargo clean
./build.sh
```

### Runtime Fails: Command Not Found

```bash
# Check if binary is in EFI drivers
ls -la efi/drivers/fileman*

# Check if init tries to launch it
grep -n "fileman" userspace/services/init/src/main.rs

# Check if it has correct permissions
file efi/drivers/fileman
```

### Runtime Fails: Permissions

Fileman uses kernel's permission checking. If you get EACCES:
- May be running as low-privilege user
- Check file/directory ownership
- Verify read/write permissions

### Runtime Fails: Filesystem

```bash
# Verify kernel filesystem is initialized
fileman:/> ls /
# Should show directories

# Check disk space
fileman:/> ls /dev
# Should show block devices

# Try writing to /tmp
fileman:/> mkdir /tmp/test
# Should succeed
```

## Version Management

### Semantic Versioning

- **0.1.x** - Initial release (current)
  - Basic file operations
  - Single-threaded CLI
  - No advanced features

- **0.2.x** - Planned
  - Interactive shell
  - Advanced options (-r, -la, etc)
  - Better error messages

- **0.3.x** - Future
  - Plugin system
  - Scripting support
  - Network filesystem

### Changelog

**v0.1.0** (2026-02-20)
- ✅ All commands implemented
- ✅ Full error handling
- ✅ Production-ready code
- ✅ Complete documentation

## Contributing

To extend fileman:

1. **Add New Command**:
   - Add handler in `src/main.rs`
   - Update `execute_command()` dispatcher
   - Document in `README.md`

2. **Improve Error Handling**:
   - Add error variant to `error.rs`
   - Map to POSIX code
   - Update exit codes

3. **Optimize Performance**:
   - Profile with flamegraph
   - Reduce allocations
   - Improve algorithm complexity

4. **Write Tests**:
   - Add unit tests in `error.rs` and `fs.rs`
   - Add integration tests in QEMU
   - Document test procedures

## Support & Documentation

- **User Guide**: `README.md`
- **Quick Start**: `QUICKSTART.md`
- **Build Info**: `BUILD.md`
- **Architecture**: `ARCHITECTURE.md`
- **This File**: `INTEGRATION.md`

## Related Projects

- **Atom OS Kernel**: `kernel/` directory
- **Init Process**: `userspace/services/init/`
- **Syscall Wrappers**: `userspace/libs/syscall/`
- **Terminal Driver**: `userspace/drivers/terminal/`

## Contact & Issues

File issues on the Atom OS GitHub repository:
- Title: `[fileman] Issue description`
- Include version: `fileman --version`
- Provide reproduction steps
- Attach relevant logs

---

**Version**: 1.0  
**Last Updated**: 2026-02-20  
**Maintained By**: Atom OS Development Team
