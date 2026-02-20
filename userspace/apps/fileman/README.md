# Fileman - File Manager for Atom OS

Production-ready file manager application for Atom OS userspace.

## Features

### Commands

- **pwd** - Print working directory
- **cd [path]** - Change directory (default: root)
- **ls [path]** - List directory contents with file sizes
- **cat <file>** - Display file contents (UTF-8)
- **mkdir <path>** - Create directory
- **rm [-r] <path>** - Remove file (or directory with -r flag)
- **mv <src> <dst>** - Move/rename files and directories
- **cp <src> <dst>** - Copy files
- **help** - Show command reference
- **exit** - Exit application

### Path Handling

- **Absolute paths**: `/home/user/file.txt`
- **Relative paths**: `../parent/file.txt`
- **Normalization**: Automatic path cleanup with `.` and `..` support
- **Validation**: Path length checking (max 4096 bytes)

### Error Handling

- Complete POSIX-like error codes
- User-friendly error messages
- Path existence validation
- File type checking
- Permission errors with context

## Building

### Prerequisites

```bash
# Ensure Rust is installed (nightly for x86_64-unknown-uefi)
rustup default nightly
rustup target add x86_64-unknown-uefi
```

### Build Command

```bash
# From workspace root
cargo build --release \
  -p fileman \
  --target=x86_64-unknown-uefi

# Or from fileman directory
cd userspace/apps/fileman
cargo build --release --target=x86_64-unknown-uefi
```

### Output

Built binary location:
```
target/x86_64-unknown-uefi/release/fileman
```

### Integration into EFI Build

To include fileman in the final EFI image:

```bash
# Using elf2atxf tool to convert to Atom format
./tools/elf2atxf/target/release/elf2atxf \
  target/x86_64-unknown-uefi/release/fileman \
  -o efi/drivers/fileman.atxf

# Then reference in boot configuration
```

## Usage

### Interactive Shell

```
$ fileman
=================================
Fileman - File Manager for Atom OS
Type 'help' for available commands
=================================
fileman:/> pwd
/
fileman:/> ls
etc/
home/
tmp/
fileman:/> cd home
fileman:/home> mkdir userdata
mkdir: created directory 'userdata'
fileman:/home> ls
userdata/
fileman:/home> cat /etc/hostname
atomos
fileman:/home> exit
```

### Programmatic Usage

Include fileman syscalls in other applications:

```rust
use fileman::fs::{Dir, File, FileMode, FsOps, FsQuery};

// List directory
let dir = Dir::open("/home")?;
for entry in dir.list()? {
    println!("{}", entry.name);
}

// Copy file
FsOps::copy("/source.txt", "/dest.txt")?;

// Check if exists
if FsQuery::exists("/tmp/cache")? {
    println!("Cache exists");
}
```

## Architecture

### Modules

#### `main.rs`
- CLI interface and command dispatcher
- Interactive shell loop
- I/O handling

#### `error.rs`
- Error types and mapping
- Path validation
- User-friendly messages
- Exit codes

#### `fs.rs`
- High-level filesystem abstractions
- File/Dir structs with RAII
- FsOps for operations (mkdir, rm, cp, mv)
- FsQuery for inspection

### Design Principles

1. **No simplifications** - Full error handling with POSIX codes
2. **Production-ready** - Memory management, resource cleanup via RAII
3. **Syscall only** - Uses only `atom_syscall::fs` for all operations
4. **Type-safe** - Rust's type system prevents many errors
5. **Efficient** - 64KB buffers for I/O, sorted directory listings

### Path Normalization

The path normalization engine handles:
- Leading/trailing slashes
- Multiple consecutive slashes
- `.` current directory references
- `..` parent directory references
- Absolute and relative paths
- Max length validation (4096 bytes)

### Error Mapping

Filesystem errors are mapped to POSIX exit codes:
- ENOENT (2) - File not found
- EEXIST (17) - File exists
- EISDIR (21) - Is a directory
- ENOTDIR (20) - Not a directory
- EACCES (13) - Permission denied
- ENOSPC (28) - No space left
- And 20+ other codes...

## Performance Characteristics

### I/O

- **Read buffer**: 4KB chunks
- **Write buffer**: 4KB chunks
- **Copy buffer**: 64KB for optimal throughput
- **Directory listing**: 64KB buffer for dirent parsing

### Memory

- **Heap**: 1MB allocated for application
- **Directory cache**: In-memory listing (scalable to thousands)
- **No external allocations**: Bounded memory usage

## Limitations

- Single-threaded operation
- No interactive line editing (single command per invocation)
- UTF-8 files for cat command
- Binary file handling (detected, not displayed)
- No symlink following in cp/mv

## Future Enhancements

1. Interactive shell with line editing
2. Glob pattern support (*.txt)
3. Permission management (chmod)
4. Recursive copy support
5. File search capabilities
6. Built-in text editor mode
7. Archive support (tar/gzip)
8. Network filesystem drivers

## Development

### Debugging

Set environment variable for verbose output:
```bash
FILEMAN_DEBUG=1 cargo build --release
```

### Testing

All operations are tested against real filesystem:
```bash
# Manual testing in QEMU
./build.sh
# Use terminal or fileman to test commands
```

### Code Quality

- No unsafe blocks except kernel interface
- Full error handling (no panics in user code)
- Memory-safe string handling
- Boundary checking for all buffers

## Integration Notes

### With Init Process

Add to `/userspace/services/init/src/main.rs`:
```rust
// After other services
let _fileman_pid = spawn_service("fileman");
```

### With Kernel

Requires kernel with:
- 24 filesystem syscalls (sys_fs_*)
- IPC routing to fsd
- FAT32 and AHCI drivers

### With Terminal

Can be launched as a subprocess:
```
terminal> fileman
```

## License

Part of Atom OS. Follows same license as main project.

## Building from Source

### Full Build Process

```bash
# 1. Build fileman
cd userspace/apps/fileman
cargo build --release --target=x86_64-unknown-uefi

# 2. Convert to Atom format (if needed)
cargo build --release -p elf2atxf
./tools/elf2atxf/target/release/elf2atxf \
  target/x86_64-unknown-uefi/release/fileman \
  --output efi/drivers/fileman

# 3. Build full kernel with updated boot config
./build.sh

# 4. Run in QEMU
cargo run --release
```

### Makefile Example

Create `userspace/apps/fileman/Makefile`:

```makefile
.PHONY: build clean run

build:
	cargo build --release --target=x86_64-unknown-uefi

clean:
	cargo clean

test: build
	# Run in QEMU
	cd ../.. && cargo run --release

install: build
	cp target/x86_64-unknown-uefi/release/fileman ../../efi/drivers/
```

## Support

- Documentation: See comments in source code
- Issues: File in Atom OS GitHub
- Contributions: Follow Rust code style and add tests

---

**Version**: 0.1.0  
**Status**: Production Ready  
**Last Updated**: 2026-02-20
