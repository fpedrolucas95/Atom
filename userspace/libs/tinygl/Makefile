# AtomOS libtinygl Makefile
#
# Builds TinyGL 0.4.1 as a freestanding static library (libtinygl.a) targeting
# x86_64 AtomOS userspace.  X11/GLX-specific files (glx.c, nglx.c) are
# intentionally excluded; the AtomOS compositor integration lives in
# atomos_zb_blit.c (this directory).
#
# Source tree layout (relative to this file):
#   ../tinygl_src/src/   — upstream TinyGL C sources
#   ../tinygl_src/include/ — upstream public headers (GL/gl.h, GL/oscontext.h …)
#   ./                   — AtomOS-specific add-on files
#
# Prerequisites:
#   ../libc must be built first (make -C ../libc)
#
# Usage:
#   make          — build build/libtinygl.a
#   make clean    — remove build artifacts
#   make check    — verify toolchain

# ---------------------------------------------------------------------------
# Toolchain detection  (same logic as libc/Makefile)
# ---------------------------------------------------------------------------
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

TGL_SRC  = ../tinygl_src/src
TGL_INC  = ../tinygl_src/include
LIBC_INC = ../libc/include
OUTDIR   = build

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
    -Wno-sign-compare \
    -Wno-implicit-fallthrough \
    -I$(TGL_SRC) \
    -I$(TGL_INC) \
    -I$(LIBC_INC) \
    -I. \
    -O2

# ---------------------------------------------------------------------------
# TinyGL core sources — glx.c and nglx.c are excluded (X11/NanoX not available)
# ---------------------------------------------------------------------------
TGL_SRCS = \
  $(TGL_SRC)/api.c        \
  $(TGL_SRC)/arrays.c     \
  $(TGL_SRC)/clear.c      \
  $(TGL_SRC)/clip.c       \
  $(TGL_SRC)/get.c        \
  $(TGL_SRC)/image_util.c \
  $(TGL_SRC)/init.c       \
  $(TGL_SRC)/light.c      \
  $(TGL_SRC)/list.c       \
  $(TGL_SRC)/matrix.c     \
  $(TGL_SRC)/memory.c     \
  $(TGL_SRC)/misc.c       \
  $(TGL_SRC)/msghandling.c \
  $(TGL_SRC)/select.c     \
  $(TGL_SRC)/specbuf.c    \
  $(TGL_SRC)/texture.c    \
  $(TGL_SRC)/vertex.c     \
  $(TGL_SRC)/zbuffer.c    \
  $(TGL_SRC)/zline.c      \
  $(TGL_SRC)/zmath.c      \
  $(TGL_SRC)/zraster.c    \
  $(TGL_SRC)/ztext.c      \
  $(TGL_SRC)/ztriangle.c

# AtomOS add-on: RGB565→ARGB32 blit helper
LOCAL_SRCS = atomos_zb_blit.c

ALL_SRCS = $(TGL_SRCS) $(LOCAL_SRCS)

# Derive object file paths inside build/
TGL_OBJS   = $(patsubst $(TGL_SRC)/%.c,$(OUTDIR)/tgl/%.o,$(TGL_SRCS))
LOCAL_OBJS = $(patsubst %.c,$(OUTDIR)/%.o,$(LOCAL_SRCS))
ALL_OBJS   = $(TGL_OBJS) $(LOCAL_OBJS)

.PHONY: all clean check

all: $(OUTDIR)/libtinygl.a

$(OUTDIR)/tgl:
	mkdir -p $(OUTDIR)/tgl

$(OUTDIR):
	mkdir -p $(OUTDIR)

# TinyGL upstream sources → build/tgl/
$(OUTDIR)/tgl/%.o: $(TGL_SRC)/%.c | $(OUTDIR)/tgl
	$(CC) $(CFLAGS) -c $< -o $@

# Local AtomOS sources → build/
$(OUTDIR)/%.o: %.c | $(OUTDIR)
	$(CC) $(CFLAGS) -c $< -o $@

$(OUTDIR)/libtinygl.a: $(ALL_OBJS)
	$(AR) rcs $@ $^
	@echo "Built: $@"

clean:
	rm -rf $(OUTDIR)

check:
	@which $(CC) > /dev/null || (echo "ERROR: $(CC) not found" && exit 1)
	@which $(AR) > /dev/null || (echo "ERROR: $(AR) not found" && exit 1)
	@echo "Toolchain OK: $$($(CC) --version | head -1)"
