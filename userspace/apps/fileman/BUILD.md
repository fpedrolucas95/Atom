# Fileman Build Instructions

## Quick Start

```bash
# From workspace root
./build.sh

# Or manually:
cd userspace/apps/fileman
cargo build --release --target=x86_64-unknown-uefi
```

## Detailed Build Process

### Step 1: Verify Dependencies

Ensure you have:
- Rust nightly toolchain
- x86_64-unknown-uefi target
- atom_syscall library available

Check:
```bash
rustc --version  # Should be nightly
rustup target list | grep x86_64-unknown-uefi
```

### Step 2: Build Binary

From `userspace/apps/fileman/`:

```bash
# Development build (faster compilation)
cargo build --target=x86_64-unknown-uefi

# Release build (optimized, smaller)
cargo build --release --target=x86_64-unknown-uefi
```

Output: `target/x86_64-unknown-uefi/release/fileman`

### Step 3: Integration Options

#### Option A: Copy to EFI Drivers

```bash
cp target/x86_64-unknown-uefi/release/fileman \
   ../../efi/drivers/fileman.atxf
```

#### Option B: Use elf2atxf Tool

Convert ELF to Atom format:

```bash
# Build the converter if not already built
cd tools/elf2atxf
cargo build --release
cd ../..

# Convert fileman
./tools/elf2atxf/target/release/elf2atxf \
  userspace/apps/fileman/target/x86_64-unknown-uefi/release/fileman \
  -o efi/drivers/fileman
```

### Step 4: Boot Integration

Update init service to launch fileman if desired:

File: `userspace/services/init/src/main.rs`

```rust
// In Phase 2 (after UI shell)
log("[Phase 2] Spawning fileman...");
let _fileman_pid = spawn_service("fileman");
```

### Step 5: Build Full Image

```bash
# From workspace root
./build.sh
```

This builds:
- Kernel
- All services and drivers
- All apps including fileman

## Troubleshooting

### Compilation Errors

**Error: "cannot find crate `atom_syscall`"**
- Ensure `Cargo.toml` has correct path dependencies
- Check that dependency paths exist

**Error: "target 'x86_64-unknown-uefi' not installed"**
```bash
rustup target add x86_64-unknown-uefi
```

**Error: "linker error: undefined reference"**
- Verify kernel is building successfully
- Check that symbols are exported

### Linker Issues

If you get linker errors during build:

```bash
# Verify target is installed
rustup target list | grep uefi

# Try clean build
cargo clean
cargo build --release --target=x86_64-unknown-uefi
```

### Size Issues

If binary is too large:

```bash
# Enable LTO in Cargo.toml [profile.release]
lto = true
opt-level = "z"

# Rebuild
cargo build --release --target=x86_64-unknown-uefi -Z build-std=core,alloc
```

## Build Variants

### Debug Build (Fast Compilation)

```bash
cargo build --target=x86_64-unknown-uefi
```

Size: ~2-3 MB  
Compile time: ~10-15 seconds

### Release Build (Optimized)

```bash
cargo build --release --target=x86_64-unknown-uefi
```

Size: ~100-300 KB  
Compile time: ~30-60 seconds

### Minimal Build

For embedded scenarios:

```bash
RUSTFLAGS="-C opt-level=z -C lto=fat" \
  cargo build --release --target=x86_64-unknown-uefi
```

## Testing Build

### Verify Binary

```bash
# Check if binary was created
ls -lh target/x86_64-unknown-uefi/release/fileman

# Check architecture
file target/x86_64-unknown-uefi/release/fileman
# Should output: ELF 64-bit LSB executable
```

### Test in QEMU

```bash
# Run full system
./build.sh
cargo run --release

# In QEMU terminal/shell
$ fileman
fileman:/> help
```

### Manual Test Commands

```bash
# List root directory
fileman> ls /

# Create test directory
fileman> mkdir /tmp/fileman_test

# Create test file
fileman> echo "test" > /tmp/test.txt  # via shell

# Display file
fileman> cat /tmp/test.txt

# Change directory
fileman> cd /tmp

# Print working directory
fileman> pwd
/tmp

# Copy file
fileman> cp test.txt test_copy.txt

# List directory
fileman> ls .

# Rename file
fileman> mv test_copy.txt backup.txt

# Remove file
fileman> rm backup.txt

# Exit
fileman> exit
```

## CI/CD Integration

### GitHub Actions Example

```yaml
name: Build Fileman

on: [push, pull_request]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: nightly
          target: x86_64-unknown-uefi
          override: true
      
      - name: Build fileman
        run: |
          cd userspace/apps/fileman
          cargo build --release --target=x86_64-unknown-uefi
      
      - name: Upload artifact
        uses: actions/upload-artifact@v2
        with:
          name: fileman
          path: userspace/apps/fileman/target/x86_64-unknown-uefi/release/fileman
```

## Build Performance

Typical build times on modern hardware:

| Scenario | Time |
|----------|------|
| Clean debug build | 15-20s |
| Clean release build | 40-60s |
| Incremental change debug | 2-5s |
| Incremental change release | 10-15s |
| Full kernel rebuild | 2-5 minutes |

## Binary Size

Typical compiled sizes:

| Config | Size |
|--------|------|
| Debug | 2-3 MB |
| Release | 200-300 KB |
| Release + LTO | 100-150 KB |

## Environment Variables

Control build behavior:

```bash
# Verbose build output
RUST_LOG=debug cargo build

# Show all warnings
RUSTFLAGS="-W warnings" cargo build

# Optimization level
RUSTFLAGS="-C opt-level=3" cargo build --release
```

## Makefile Template

Create `userspace/apps/fileman/Makefile`:

```makefile
.PHONY: build clean test help

TARGET ?= x86_64-unknown-uefi
RELEASE ?= 1

ifeq ($(RELEASE),1)
BUILD_ARGS = --release
OUT_DIR = target/$(TARGET)/release
else
OUT_DIR = target/$(TARGET)/debug
endif

FILEMAN_BIN = $(OUT_DIR)/fileman

help:
	@echo "Fileman Build Targets"
	@echo "====================="
	@echo "make build      - Build fileman binary"
	@echo "make clean      - Clean build artifacts"
	@echo "make test       - Run in QEMU"
	@echo "make install    - Copy to EFI drivers"

build: $(FILEMAN_BIN)

$(FILEMAN_BIN):
	cargo build $(BUILD_ARGS) --target=$(TARGET)

clean:
	cargo clean

test: build
	cd ../.. && ./build.sh && cargo run --release

install: build
	cp $(FILEMAN_BIN) ../../efi/drivers/fileman

.PHONY: all
```

Build with:
```bash
make build RELEASE=1
make test
make install
```

## Notes

- All commands should complete in under 1 second
- Fileman uses no unsafe code except at FFI boundary
- Memory usage is bounded (1 MB heap)
- No external dependencies except atom_syscall
- Fully self-contained application
