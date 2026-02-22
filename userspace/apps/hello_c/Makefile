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

CC  = gcc
LD  = ld

LIBC_DIR  = ../../libs/libc
LIBC      = $(LIBC_DIR)/build/libc.a
CRT0      = $(LIBC_DIR)/build/crt0.o
LDSCRIPT  = ../../userspace.ld
ELF2ATXF  = ../../../tools/elf2atxf/target/$(shell rustc -vV 2>/dev/null | sed -n 's/host: //p')/release/elf2atxf

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
    -I$(LIBC_DIR)/include \
    -O2

LDFLAGS = \
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
	    $(ELF2ATXF) $(TARGET).elf $(TARGET).atxf && echo "Built: $(TARGET).atxf"; \
	else \
	    echo "WARNING: elf2atxf not found at $(ELF2ATXF); skipping ATXF conversion"; \
	fi

clean:
	rm -f $(OBJS) $(TARGET).elf $(TARGET).atxf
