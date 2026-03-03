# Makefile — DOOM para AtomOS (via doomgeneric)
#
# Porta o Doom clássico para o desktop environment do AtomOS usando
# doomgeneric como camada de abstração portável e TinyGL para a
# integração com o compositor.
#
# ───────────────────────────────────────────────────────────────────────
# PRÉ-REQUISITOS
# ───────────────────────────────────────────────────────────────────────
#  1. Clonar doomgeneric na pasta esperada:
#       git clone https://github.com/ozkl/doomgeneric \
#           userspace/libs/doomgeneric_src
#     (a variável DOOMGENERIC_SRC aponta para o subdiretório interno)
#
#  2. Obter um IWAD (arquivo de dados do Doom):
#       doom1.wad  — shareware gratuito (Doom 1 episódio 1)
#       doom.wad   — comercial (Doom 1 completo)
#     Coloque o WAD em efi/apps/user/ (ou outro caminho acessível pelo FS).
#
#  3. Build das dependências:
#       make -C ../../libs/libc
#       make -C ../../libs/tinygl
#
#  4. elf2atxf compilado:
#       cd ../../../tools/elf2atxf && cargo build --release \
#           --target $(rustc -vV | sed -n 's/host: //p')
#
# Uso:
#   make              — compila doom.elf e doom.atxf
#   make clean        — remove artefatos

# ---------------------------------------------------------------------------
# Resolução interna do Doom e da janela
#   DOOM_W × DOOM_H  — o que o software renderer do Doom produz
#   WIN_W  × WIN_H   — tamanho da janela do compositor (2× DOOM, 640×400)
# ---------------------------------------------------------------------------
DOOM_W ?= 320
DOOM_H ?= 200

# ---------------------------------------------------------------------------
# Localização do código-fonte do doomgeneric
# ---------------------------------------------------------------------------
DOOMGENERIC_SRC ?= ../../libs/doomgeneric_src/doomgeneric

# ---------------------------------------------------------------------------
# Detecção do toolchain (mesma lógica que tinygl_demo e libc)
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
# Diretórios de dependências
# ---------------------------------------------------------------------------
LIBC_DIR   = ../../libs/libc
TINYGL_DIR = ../../libs/tinygl
TINYGL_SRC = ../../libs/tinygl_src

LIBC      = $(LIBC_DIR)/build/libc.a
CRT0      = $(LIBC_DIR)/build/crt0.o
LIBTINYGL = $(TINYGL_DIR)/build/libtinygl.a
LDSCRIPT  = ../../userspace.ld

ELF2ATXF  = ../../../tools/elf2atxf/target/$(shell rustc -vV 2>/dev/null \
             | sed -n 's/host: //p')/release/elf2atxf

OUTDIR = build

# ---------------------------------------------------------------------------
# Flags de compilação
#
# Flags importantes para o doomgeneric:
#   -DDOOMGENERIC_RESX / -DDOOMGENERIC_RESY
#       Resolução do framebuffer produzida pelo Doom (320×200 clássico).
#   -DNOMIXER
#       Desabilita mistura de áudio (AtomOS ainda não tem driver de som).
#   -DFEATURE_MULTIPLAYER=0 / -DNET_DISABLED
#       Desabilita código de rede (não necessário para o port básico).
#   -Wno-*
#       O código-fonte legado do Doom produz vários avisos inofensivos.
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
    -std=c99 \
    -DDOOMGENERIC_RESX=$(DOOM_W) \
    -DDOOMGENERIC_RESY=$(DOOM_H) \
    -DNOMIXER \
    -DNO_JOYSTICK \
    -O2 \
    -I$(LIBC_DIR)/include \
    -I$(TINYGL_SRC)/include \
    -I$(TINYGL_DIR) \
    -I$(DOOMGENERIC_SRC) \
    -I. \
    -Wno-unused-parameter \
    -Wno-sign-compare \
    -Wno-implicit-fallthrough \
    -Wno-misleading-indentation \
    -Wno-missing-field-initializers

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
# Fontes: todos os .c do doomgeneric + nosso backend AtomOS
# ---------------------------------------------------------------------------
DOOM_SRCS  := $(wildcard $(DOOMGENERIC_SRC)/*.c)
LOCAL_SRCS := atomos_backend.c
ALL_SRCS   := $(DOOM_SRCS) $(LOCAL_SRCS)

DOOM_OBJS  := $(patsubst $(DOOMGENERIC_SRC)/%.c,$(OUTDIR)/doom/%.o,$(DOOM_SRCS))
LOCAL_OBJS := $(patsubst %.c,$(OUTDIR)/%.o,$(LOCAL_SRCS))
ALL_OBJS   := $(DOOM_OBJS) $(LOCAL_OBJS)

TARGET = doom

.PHONY: all clean check-src

all: check-src $(TARGET).atxf

# Verificar se o código-fonte do doomgeneric está disponível
check-src:
	@if [ ! -d "$(DOOMGENERIC_SRC)" ]; then \
	    echo ""; \
	    echo "ERRO: doomgeneric não encontrado em $(DOOMGENERIC_SRC)"; \
	    echo ""; \
	    echo "  Clone o repositório com:"; \
	    echo "    git clone https://github.com/ozkl/doomgeneric \\"; \
	    echo "        userspace/libs/doomgeneric_src"; \
	    echo ""; \
	    exit 1; \
	fi

$(OUTDIR)/doom:
	mkdir -p $(OUTDIR)/doom

$(OUTDIR):
	mkdir -p $(OUTDIR)

# Compilar fontes do doomgeneric
$(OUTDIR)/doom/%.o: $(DOOMGENERIC_SRC)/%.c | $(OUTDIR)/doom
	$(CC) $(CFLAGS) -c $< -o $@

# Compilar backend AtomOS
$(OUTDIR)/%.o: %.c | $(OUTDIR)
	$(CC) $(CFLAGS) -c $< -o $@

# Linkar: crt0 + doomgeneric + backend + libtinygl + libc
# Ordem de link: libtinygl antes de libc (resolve malloc/free)
$(TARGET).elf: $(CRT0) $(ALL_OBJS) $(LIBTINYGL) $(LIBC)
	$(LD) $(LDFLAGS) -o $@ $(CRT0) $(ALL_OBJS) $(LIBTINYGL) $(LIBC)

$(TARGET).atxf: $(TARGET).elf
	@if [ -x "$(ELF2ATXF)" ]; then \
	    $(ELF2ATXF) $(TARGET).elf $(TARGET).atxf && echo "Built: $(TARGET).atxf"; \
	else \
	    echo "AVISO: elf2atxf não encontrado; gerou apenas $(TARGET).elf"; \
	fi

clean:
	rm -rf $(OUTDIR) $(TARGET).elf $(TARGET).atxf
