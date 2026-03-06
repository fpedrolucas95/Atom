# AtomOS libsvg Makefile
#
# Constrói libsvg (ravhed/libsvg) como biblioteca estática freestanding
# para userspace AtomOS.

CROSS_GCC := $(shell command -v x86_64-elf-gcc 2>/dev/null)
ifneq ($(CROSS_GCC),)
  CC         = x86_64-elf-gcc
  AR         = x86_64-elf-ar
  ARCH_FLAGS = -march=x86-64
else
  IS_CLANG   := $(findstring clang,$(shell gcc --version 2>&1))
  ifneq ($(IS_CLANG),)
    CC         = clang
    AR         = ar
    ARCH_FLAGS = -target x86_64-unknown-none-elf
  else
    CC         = gcc
    AR         = ar
    ARCH_FLAGS = -march=x86-64
  endif
endif

SVG_SRC   = ../../../third_party/libsvg_upstream/libsvg
LIBC_INC  = ../libc/include
XML_INC   = ../../../third_party/libxml2_upstream/include
OUTDIR    = build

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
    -Wall -Wextra \
    -Wno-unused-parameter \
    -I$(SVG_SRC)/src \
    -I$(SVG_SRC)/src \
    -I$(XML_INC) \
    -I$(LIBC_INC) \
    -I. \
    -D_POSIX_C_SOURCE=200809L \
    -O2

SVG_SRCS = \
    $(SVG_SRC)/src/svg.c \
    $(SVG_SRC)/src/svg_ascii.c \
    $(SVG_SRC)/src/svg_hash.c

SVG_OBJS = $(patsubst $(SVG_SRC)/src/%.c,$(OUTDIR)/%.o,$(SVG_SRCS))

.PHONY: all clean check

all: $(OUTDIR)/libsvg_atom.a

$(OUTDIR):
	mkdir -p $(OUTDIR)

$(OUTDIR)/%.o: $(SVG_SRC)/src/%.c | $(OUTDIR)
	$(CC) $(CFLAGS) -c $< -o $@

$(OUTDIR)/libsvg_atom.a: $(SVG_OBJS)
	$(AR) rcs $@ $^
	@echo "Built: $@"

clean:
	rm -rf $(OUTDIR)

check:
	@which $(CC) > /dev/null || (echo "ERROR: $(CC) not found" && exit 1)
	@which $(AR) > /dev/null || (echo "ERROR: $(AR) not found" && exit 1)
	@echo "Toolchain OK: $$($(CC) --version | head -1)"

