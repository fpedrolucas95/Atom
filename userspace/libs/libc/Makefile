# AtomOS libc Makefile
#
# Builds a freestanding C library targeting x86_64 AtomOS.
# The result is a static archive (libc.a) plus a startup object (crt0.o).
#
# Usage:
#   make          — build libc.a and crt0.o in build/
#   make clean    — remove build artifacts
#   make check    — verify toolchain is available

# ---------------------------------------------------------------------------
# Toolchain detection
# Priority: x86_64-elf-gcc (bare-metal cross) > clang (Apple/LLVM) > gcc
# On macOS 'gcc' is Apple clang which requires an explicit ELF target triple
# instead of -march=x86-64 and generates ELF when -target is set.
# Install a proper cross-compiler with: brew install x86_64-elf-gcc
# ---------------------------------------------------------------------------
CROSS_GCC := $(shell command -v x86_64-elf-gcc 2>/dev/null)
ifneq ($(CROSS_GCC),)
  CC         = x86_64-elf-gcc
  AR         = x86_64-elf-ar
  ARCH_FLAGS = -march=x86-64
else
  IS_CLANG   := $(findstring clang,$(shell gcc --version 2>&1))
  ifneq ($(IS_CLANG),)
    # Apple clang / system LLVM clang — emit ELF via explicit target triple
    CC         = clang
    AR         = ar
    ARCH_FLAGS = -target x86_64-unknown-none-elf
  else
    CC         = gcc
    AR         = ar
    ARCH_FLAGS = -march=x86-64
  endif
endif

ARCH   = x86_64

# Freestanding compile flags for AtomOS userspace
CFLAGS = \
    $(ARCH_FLAGS) \
    -m64 \
    -ffreestanding \
    -fno-stack-protector \
    -fPIE \
    -fPIC \
    -mno-red-zone \
    -nostdlib \
    -nostdinc \
    -std=c11 \
    -Wall \
    -Wextra \
    -Wno-unused-parameter \
    -Iinclude \
    -O2

# Same flags for assembling .S startup files
ASFLAGS = \
    $(ARCH_FLAGS) \
    -m64 \
    -ffreestanding \
    -nostdlib

OUTDIR = build

SRCS = \
    src/string.c \
    src/stdlib.c \
    src/stdio.c \
    src/ctype.c \
    src/errno.c \
    src/assert.c \
    src/math.c \
    src/unistd.c \
    src/time.c

OBJS = $(patsubst src/%.c,$(OUTDIR)/%.o,$(SRCS))

.PHONY: all clean check

all: $(OUTDIR)/libc.a $(OUTDIR)/crt0.o

$(OUTDIR):
	mkdir -p $(OUTDIR)

$(OUTDIR)/%.o: src/%.c | $(OUTDIR)
	$(CC) $(CFLAGS) -c $< -o $@

$(OUTDIR)/crt0.o: crt/crt0.S | $(OUTDIR)
	$(CC) $(ASFLAGS) -c $< -o $@

$(OUTDIR)/libc.a: $(OBJS) | $(OUTDIR)
	$(AR) rcs $@ $^
	@echo "Built: $@"

clean:
	rm -rf $(OUTDIR)

check:
	@which $(CC) > /dev/null || (echo "ERROR: $(CC) not found" && exit 1)
	@which $(AR) > /dev/null || (echo "ERROR: $(AR) not found" && exit 1)
	@echo "Toolchain OK: $$($(CC) --version | head -1)"
