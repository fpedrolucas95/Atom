# Makefile for doom — AtomOS doomgeneric port
#
# Builds doomgeneric (all core Doom C files) together with the AtomOS
# platform layer (doomgeneric_atom.c) and links against libc.
#
# Prerequisites:
#   1. userspace/libs/libc must be built first:
#        make -C ../../libs/libc
#   2. elf2atxf tool must be built for the host:
#        cd tools/elf2atxf && cargo build --release --target $(host triple)
#
# Usage:
#   make          — builds doom.elf and doom.atxf
#   make clean    — removes build artifacts

# ---------------------------------------------------------------------------
# Toolchain detection  (identical logic to tinygl_demo/Makefile)
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
DOOM_SRC   = ../../libs/doomgeneric_src/doomgeneric

LIBC     = $(LIBC_DIR)/build/libc.a
CRT0     = $(LIBC_DIR)/build/crt0.o
LDSCRIPT = ../../userspace.ld

ELF2ATXF = ../../../tools/elf2atxf/target/$(shell rustc -vV 2>/dev/null | sed -n 's/host: //p')/release/elf2atxf

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
    -std=gnu89 \
    -O2 \
    -Wall \
    -Wno-unused-function \
    -Wno-unused-variable \
    -Wno-unused-parameter \
    -Wno-incompatible-pointer-types \
    -Wno-unused-but-set-variable \
    -DNORMALUNIX \
    -DLINUX \
    -D_DEFAULT_SOURCE \
    -I$(LIBC_DIR)/include \
    -I$(DOOM_SRC)

# ---------------------------------------------------------------------------
# Linker flags
# ---------------------------------------------------------------------------
LDFLAGS = \
    $(LDFLAGS_FLAVOR) \
    -T $(LDSCRIPT) \
    -nostdlib \
    --no-pie \
    -static

# ---------------------------------------------------------------------------
# Source files
#
# All core Doom sources from doomgeneric (mirror of SRC_DOOM in upstream
# Makefile) minus the upstream X11 platform layer (doomgeneric_xlib.c),
# plus our AtomOS platform layer.
# ---------------------------------------------------------------------------
DOOM_SRCS = \
    $(DOOM_SRC)/dummy.c \
    $(DOOM_SRC)/am_map.c \
    $(DOOM_SRC)/doomdef.c \
    $(DOOM_SRC)/doomstat.c \
    $(DOOM_SRC)/dstrings.c \
    $(DOOM_SRC)/d_event.c \
    $(DOOM_SRC)/d_items.c \
    $(DOOM_SRC)/d_iwad.c \
    $(DOOM_SRC)/d_loop.c \
    $(DOOM_SRC)/d_main.c \
    $(DOOM_SRC)/d_mode.c \
    $(DOOM_SRC)/d_net.c \
    $(DOOM_SRC)/f_finale.c \
    $(DOOM_SRC)/f_wipe.c \
    $(DOOM_SRC)/g_game.c \
    $(DOOM_SRC)/hu_lib.c \
    $(DOOM_SRC)/hu_stuff.c \
    $(DOOM_SRC)/info.c \
    $(DOOM_SRC)/i_cdmus.c \
    $(DOOM_SRC)/i_endoom.c \
    $(DOOM_SRC)/i_joystick.c \
    $(DOOM_SRC)/i_scale.c \
    $(DOOM_SRC)/i_sound.c \
    $(DOOM_SRC)/i_system.c \
    $(DOOM_SRC)/i_timer.c \
    $(DOOM_SRC)/memio.c \
    $(DOOM_SRC)/m_argv.c \
    $(DOOM_SRC)/m_bbox.c \
    $(DOOM_SRC)/m_cheat.c \
    $(DOOM_SRC)/m_config.c \
    $(DOOM_SRC)/m_controls.c \
    $(DOOM_SRC)/m_fixed.c \
    $(DOOM_SRC)/m_menu.c \
    $(DOOM_SRC)/m_misc.c \
    $(DOOM_SRC)/m_random.c \
    $(DOOM_SRC)/p_ceilng.c \
    $(DOOM_SRC)/p_doors.c \
    $(DOOM_SRC)/p_enemy.c \
    $(DOOM_SRC)/p_floor.c \
    $(DOOM_SRC)/p_inter.c \
    $(DOOM_SRC)/p_lights.c \
    $(DOOM_SRC)/p_map.c \
    $(DOOM_SRC)/p_maputl.c \
    $(DOOM_SRC)/p_mobj.c \
    $(DOOM_SRC)/p_plats.c \
    $(DOOM_SRC)/p_pspr.c \
    $(DOOM_SRC)/p_saveg.c \
    $(DOOM_SRC)/p_setup.c \
    $(DOOM_SRC)/p_sight.c \
    $(DOOM_SRC)/p_spec.c \
    $(DOOM_SRC)/p_switch.c \
    $(DOOM_SRC)/p_telept.c \
    $(DOOM_SRC)/p_tick.c \
    $(DOOM_SRC)/p_user.c \
    $(DOOM_SRC)/r_bsp.c \
    $(DOOM_SRC)/r_data.c \
    $(DOOM_SRC)/r_draw.c \
    $(DOOM_SRC)/r_main.c \
    $(DOOM_SRC)/r_plane.c \
    $(DOOM_SRC)/r_segs.c \
    $(DOOM_SRC)/r_sky.c \
    $(DOOM_SRC)/r_things.c \
    $(DOOM_SRC)/sha1.c \
    $(DOOM_SRC)/sounds.c \
    $(DOOM_SRC)/statdump.c \
    $(DOOM_SRC)/st_lib.c \
    $(DOOM_SRC)/st_stuff.c \
    $(DOOM_SRC)/s_sound.c \
    $(DOOM_SRC)/tables.c \
    $(DOOM_SRC)/v_video.c \
    $(DOOM_SRC)/wi_stuff.c \
    $(DOOM_SRC)/w_checksum.c \
    $(DOOM_SRC)/w_file.c \
    $(DOOM_SRC)/w_main.c \
    $(DOOM_SRC)/w_wad.c \
    $(DOOM_SRC)/z_zone.c \
    $(DOOM_SRC)/w_file_stdc.c \
    $(DOOM_SRC)/i_input.c \
    $(DOOM_SRC)/i_video.c \
    $(DOOM_SRC)/doomgeneric.c

# AtomOS platform layer (replaces doomgeneric_xlib.c)
PLATFORM_SRC = doomgeneric_atom.c

ALL_SRCS = $(DOOM_SRCS) $(PLATFORM_SRC)

# Build object files in a local build/ subdirectory to avoid polluting the
# doomgeneric_src tree.
OBJDIR = build

# Derive object paths relative to OBJDIR:
# For doom sources, flatten the basename (all names are unique in the list above)
DOOM_OBJS    = $(patsubst %.c,$(OBJDIR)/doom_%.o,$(notdir $(DOOM_SRCS)))
PLATFORM_OBJ = $(OBJDIR)/$(PLATFORM_SRC:.c=.o)
ALL_OBJS     = $(DOOM_OBJS) $(PLATFORM_OBJ)

TARGET = doom

.PHONY: all clean

all: $(TARGET).atxf

# Create object directory
$(OBJDIR):
	mkdir -p $(OBJDIR)

# Compile doom source files (from doomgeneric_src tree)
$(OBJDIR)/doom_%.o: $(DOOM_SRC)/%.c | $(OBJDIR)
	$(CC) $(CFLAGS) -c $< -o $@

# Compile our own platform layer
$(OBJDIR)/doomgeneric_atom.o: doomgeneric_atom.c | $(OBJDIR)
	$(CC) $(CFLAGS) -c $< -o $@

# Link: crt0 + all objects + libc
$(TARGET).elf: $(CRT0) $(ALL_OBJS) $(LIBC)
	$(LD) $(LDFLAGS) -o $@ $(CRT0) $(ALL_OBJS) $(LIBC)

# Convert ELF → ATXF
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
	rm -rf $(OBJDIR) $(TARGET).elf $(TARGET).atxf
