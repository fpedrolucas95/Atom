# Makefile for hello_c — AtomOS C demo application
#
# This Makefile builds a C program using the AtomOS libc.  The resulting ELF
# is then converted to ATXF format by elf2atxf for loading by the kernel.
#
# Prerequisites:
#   - userspace/libs/libc must be built first (run `make` in that directory)
#   - elf2atxf tool must be built (tools/elf2atxf)
#   - The userspace linker script is at userspace/userspace.ld
#
# Usage:
#   make              — build hello_c.elf and hello_c.atxf
#   make clean        — remove build artifacts

# ---------------------------------------------------------------------------
# Toolchain detection
# Same logic as userspace/libs/libc/Makefile.
# On macOS 'gcc' == Apple clang; use -target to emit ELF.
# Install a proper cross-compiler with: brew install x86_64-elf-gcc
# ---------------------------------------------------------------------------
CROSS_GCC := $(shell command -v x86_64-elf-gcc 2>/dev/null)
ifneq ($(CROSS_GCC),)
  CC         = x86_64-elf-gcc
  ARCH_FLAGS = -march=x86-64
else
  IS_CLANG   := $(findstring clang,$(shell gcc --version 2>&1))
  ifneq ($(IS_CLANG),)
    CC         = clang
    ARCH_FLAGS = -target x86_64-unknown-none-elf
  else
    CC         = gcc
    ARCH_FLAGS = -march=x86-64
  endif
endif

# Linker: x86_64-elf-ld > rust-lld (ELF flavor) > system ld
CROSS_LD := $(shell command -v x86_64-elf-ld 2>/dev/null)
ifneq ($(CROSS_LD),)
  LD       = x86_64-elf-ld
  LDFLAGS_FLAVOR =
else
  RUST_LLD := $(shell find ~/.rustup/toolchains/nightly-*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -1)
  ifneq ($(RUST_LLD),)
    LD             = $(RUST_LLD)
    LDFLAGS_FLAVOR = -flavor ld
  else
    LD             = ld
    LDFLAGS_FLAVOR =
  endif
endif

LIBC_DIR  = ../../libs/libc
LIBC      = $(LIBC_DIR)/build/libc.a
CRT0      = $(LIBC_DIR)/build/crt0.o
LDSCRIPT  = ../../userspace.ld
ELF2ATXF  = ../../../tools/elf2atxf/target/$(shell rustc -vV 2>/dev/null | sed -n 's/host: //p')/release/elf2atxf

CFLAGS = \
    $(ARCH_FLAGS) \
    -m64 \
    -ffreestanding \
    -fno-stack-protector \
    -fno-pie \
    -fno-pic \
    -mno-red-zone \
    -nostdlib \
    -nostdinc \
    -std=c11 \
    -Wall \
    -Wextra \
    -I$(LIBC_DIR)/include \
    -O2

LDFLAGS = \
    $(LDFLAGS_FLAVOR) \
    -T $(LDSCRIPT) \
    -nostdlib \
    --no-pie \
    -static

SRCS  = main.c
OBJS  = $(SRCS:.c=.o)
TARGET = hello_c

.PHONY: all clean

all: $(TARGET).atxf

%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

$(TARGET).elf: $(CRT0) $(OBJS) $(LIBC)
	$(LD) $(LDFLAGS) -o $@ $(CRT0) $(OBJS) $(LIBC)

$(TARGET).atxf: $(TARGET).elf
	@if [ -x "$(ELF2ATXF)" ]; then \
      if [ -f "ico.svg" ]; then \
          $(ELF2ATXF) --icon ico.svg $(TARGET).elf $(TARGET).atxf && echo "Built: $(TARGET).atxf (icon embedded)"; \
      else \
          $(ELF2ATXF) $(TARGET).elf $(TARGET).atxf && echo "Built: $(TARGET).atxf"; \
      fi; \
	else \
	    echo "WARNING: elf2atxf not found at $(ELF2ATXF); skipping ATXF conversion"; \
	fi

clean:
	rm -f $(OBJS) $(TARGET).elf $(TARGET).atxf
