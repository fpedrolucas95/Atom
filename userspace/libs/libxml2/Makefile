# AtomOS libxml2 Makefile
#
# Constrói libxml2 como biblioteca estática freestanding para userspace AtomOS.
# Usa os fontes de third_party/libxml2_upstream.

# ---------------------------------------------------------------------------
# Toolchain (mesma lógica do TinyGL/libc)
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

XML_SRC  = ../../../third_party/libxml2_upstream
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
    -Wall -Wextra \
    -Wno-unused-parameter \
    -I$(XML_SRC)/include \
    -I$(XML_SRC)/include/libxml \
    -I$(LIBC_INC) \
    -I. \
    -DNO_ICONV \
    -DNO_LZMA \
    -DNO_ZLIB \
    -D_POSIX_C_SOURCE=200809L \
    -O2

# Conjunto inicial de fontes centrais do libxml2.
# Ajuste esta lista caso o link com libsvg acuse símbolos ausentes.
XML_SRCS = \
    $(XML_SRC)/buf.c \
    $(XML_SRC)/c14n.c \
    $(XML_SRC)/chvalid.c \
    $(XML_SRC)/dict.c \
    $(XML_SRC)/encoding.c \
    $(XML_SRC)/entities.c \
    $(XML_SRC)/error.c \
    $(XML_SRC)/globals.c \
    $(XML_SRC)/hash.c \
    $(XML_SRC)/list.c \
    $(XML_SRC)/nanohttp.c \
    $(XML_SRC)/parser.c \
    $(XML_SRC)/parserInternals.c \
    $(XML_SRC)/pattern.c \
    $(XML_SRC)/relaxng.c \
    $(XML_SRC)/threads.c \
    $(XML_SRC)/tree.c \
    $(XML_SRC)/uri.c \
    $(XML_SRC)/valid.c \
    $(XML_SRC)/xinclude.c \
    $(XML_SRC)/xlink.c \
    $(XML_SRC)/xmlIO.c \
    $(XML_SRC)/xmlmemory.c \
    $(XML_SRC)/xmlmodule.c \
    $(XML_SRC)/xmlreader.c \
    $(XML_SRC)/xmlregexp.c \
    $(XML_SRC)/xmlsave.c \
    $(XML_SRC)/xmlstring.c \
    $(XML_SRC)/xmlwriter.c \
    $(XML_SRC)/xpath.c \
    $(XML_SRC)/xpointer.c

XML_OBJS = $(patsubst $(XML_SRC)/%.c,$(OUTDIR)/%.o,$(XML_SRCS))

.PHONY: all clean check

all: $(OUTDIR)/libxml2_atom.a

$(OUTDIR):
	mkdir -p $(OUTDIR)

$(OUTDIR)/%.o: $(XML_SRC)/%.c | $(OUTDIR)
	$(CC) $(CFLAGS) -c $< -o $@

$(OUTDIR)/libxml2_atom.a: $(XML_OBJS)
	$(AR) rcs $@ $^
	@echo "Built: $@"

clean:
	rm -rf $(OUTDIR)

check:
	@which $(CC) > /dev/null || (echo "ERROR: $(CC) not found" && exit 1)
	@which $(AR) > /dev/null || (echo "ERROR: $(AR) not found" && exit 1)
	@echo "Toolchain OK: $$($(CC) --version | head -1)"

