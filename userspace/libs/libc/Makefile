# AtomOS libc Makefile
#
# Builds a freestanding C library targeting x86_64 AtomOS.
# The result is a static archive (libc.a) plus a startup object (crt0.o).
#
# Usage:
#   make          — build libc.a and crt0.o in build/
#   make clean    — remove build artifacts
#   make check    — verify toolchain is available

CC     = gcc
AR     = ar
ARCH   = x86_64

# Freestanding compile flags for AtomOS userspace
CFLAGS = \
    -m64 \
    -march=x86-64 \
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
    -Iinclude \
    -O2

# Same flags for assembling .S startup files
ASFLAGS = \
    -m64 \
    -march=x86-64 \
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
    src/unistd.c

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
