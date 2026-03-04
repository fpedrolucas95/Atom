# Makefile for tinygl_demo — AtomOS TinyGL 0.4.1 Gears demo
#
# Builds a C program using AtomOS libc + libtinygl.
# The resulting ELF is then converted to ATXF format by elf2atxf.
#
# Prerequisites:
#   1. userspace/libs/libc must be built first:
#        make -C ../../libs/libc
#   2. userspace/libs/tinygl must be built first:
#        make -C ../../libs/tinygl
#   3. elf2atxf tool must be built for the host:
#        cd tools/elf2atxf && cargo build --release --target $(rustc -vV | sed -n 's/host: //p')
#      (the --target flag is required because .cargo/config.toml sets the
#       default target to x86_64-unknown-uefi for the kernel build)
#
# Usage:
#   make              — build tinygl_demo.elf and tinygl_demo.atxf
#   make clean        — remove build artifacts

# ---------------------------------------------------------------------------
# Toolchain detection  (same logic as libc/Makefile)
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

# Linker
CROSS_LD := $(shell command -v x86_64-elf-ld 2>/dev/null)
ifneq ($(CROSS_LD),)
  LD             = x86_64-elf-ld
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

# ---------------------------------------------------------------------------
# Dependency directories
# ---------------------------------------------------------------------------
LIBC_DIR   = ../../libs/libc
TINYGL_DIR = ../../libs/tinygl
TINYGL_SRC = ../../libs/tinygl_src

LIBC         = $(LIBC_DIR)/build/libc.a
CRT0         = $(LIBC_DIR)/build/crt0.o
LIBTINYGL    = $(TINYGL_DIR)/build/libtinygl.a
LDSCRIPT     = ../../userspace.ld

ELF2ATXF     = ../../../tools/elf2atxf/target/$(shell rustc -vV 2>/dev/null | sed -n 's/host: //p')/release/elf2atxf

# ---------------------------------------------------------------------------
# Compiler flags
# ---------------------------------------------------------------------------
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
    -Wno-unused-parameter \
    -I$(LIBC_DIR)/include         \
    -I$(TINYGL_SRC)/include       \
    -I$(TINYGL_DIR)               \
    -O2

# ---------------------------------------------------------------------------
# Linker flags
# ---------------------------------------------------------------------------
LDFLAGS = \
    $(LDFLAGS_FLAVOR) \
    -T $(LDSCRIPT) \
    -nostdlib \
    --no-pie \
    -static

SRCS   = main.c
OBJS   = $(SRCS:.c=.o)
TARGET = tinygl_demo

.PHONY: all clean

all: $(TARGET).atxf

%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

# Link: crt0 + application objects + libtinygl + libc
# libtinygl must come before libc so that its malloc/free calls
# are satisfied by libc rather than being multiply-defined.
$(TARGET).elf: $(CRT0) $(OBJS) $(LIBTINYGL) $(LIBC)
	$(LD) $(LDFLAGS) -o $@ $(CRT0) $(OBJS) $(LIBTINYGL) $(LIBC)

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
