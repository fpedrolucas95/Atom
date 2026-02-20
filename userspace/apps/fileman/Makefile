# Makefile for Fileman

.PHONY: help build clean test install debug release size

TARGET := x86_64-unknown-uefi
WORKSPACE_ROOT := ../..
FILEMAN_DIR := $(shell pwd)

# Determine if release or debug
RELEASE ?= 1
ifeq ($(RELEASE),1)
  CARGO_FLAGS := --release
  BUILD_DIR := target/$(TARGET)/release
  OUT_LABEL := release
else
  CARGO_FLAGS :=
  BUILD_DIR := target/$(TARGET)/debug
  OUT_LABEL := debug
endif

BINARY := $(BUILD_DIR)/fileman
EFI_OUT := $(WORKSPACE_ROOT)/efi/drivers/fileman

.PHONY: help
help:
	@echo "Fileman Build and Installation"
	@echo "=============================="
	@echo ""
	@echo "Targets:"
	@echo "  make build       - Build fileman binary (RELEASE=1 for optimized)"
	@echo "  make clean       - Remove build artifacts"
	@echo "  make test        - Build and run in QEMU"
	@echo "  make install     - Copy binary to EFI drivers directory"
	@echo "  make debug       - Build debug version"
	@echo "  make release     - Build release version"
	@echo "  make size        - Show binary size information"
	@echo "  make help        - Show this help message"
	@echo ""
	@echo "Variables:"
	@echo "  RELEASE=0        - Build debug version (default: 1)"
	@echo "  TARGET=...       - Build target (default: x86_64-unknown-uefi)"
	@echo ""
	@echo "Examples:"
	@echo "  make build                    # Build release"
	@echo "  make build RELEASE=0          # Build debug"
	@echo "  make test                     # Test in QEMU"
	@echo "  make install                  # Install to EFI"

.PHONY: build
build: $(BINARY)
	@echo "[✓] Built fileman ($(OUT_LABEL)) - $(BUILD_DIR)"

$(BINARY):
	@echo "[*] Building fileman ($(OUT_LABEL))..."
	@cargo build $(CARGO_FLAGS) --target=$(TARGET) 2>&1

.PHONY: debug
debug:
	@echo "[*] Building debug version..."
	@RELEASE=0 $(MAKE) build

.PHONY: release
release:
	@echo "[*] Building release version..."
	@RELEASE=1 $(MAKE) build

.PHONY: clean
clean:
	@echo "[*] Cleaning build artifacts..."
	@cargo clean
	@echo "[✓] Cleaned"

.PHONY: test
test: build
	@echo "[*] Building full system and running in QEMU..."
	@cd $(WORKSPACE_ROOT) && ./build.sh && cargo run --release 2>&1 | head -50

.PHONY: install
install: release
	@echo "[*] Installing fileman to EFI drivers..."
	@mkdir -p $(dir $(EFI_OUT))
	@cp $(BINARY) $(EFI_OUT)
	@echo "[✓] Installed to $(EFI_OUT)"

.PHONY: size
size: build
	@echo "Binary size information:"
	@ls -lh $(BINARY)
	@echo ""
	@echo "Size breakdown:"
	@size $(BINARY) 2>/dev/null || echo "  (size command not available)"

.PHONY: check
check:
	@echo "[*] Checking code..."
	@cargo check --target=$(TARGET)

.PHONY: fmt
fmt:
	@echo "[*] Formatting code..."
	@cargo fmt

.PHONY: clippy
clippy:
	@echo "[*] Running clippy..."
	@cargo clippy --target=$(TARGET) -- -D warnings

.PHONY: doc
doc:
	@echo "[*] Building documentation..."
	@cargo doc --target=$(TARGET) --no-deps --open

.PHONY: run-tests
run-tests:
	@echo "[*] Running tests..."
	@cargo test --target=$(TARGET) 2>&1

.PHONY: all
all: clean build install
	@echo "[✓] Complete build and installation finished"

# Default target
.DEFAULT_GOAL := help

# Prevent parallel builds to avoid clean conflicts
.NOTPARALLEL: clean all
