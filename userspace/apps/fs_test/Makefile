# Build configuration for fs_test
CARGO = cargo
CARGO_RELEASE = release
CARGO_RELEASE_ARGS = --release

.PHONY: all build clean help

all: build

build:
	@echo "Building fs_test..."
	$(CARGO) build --manifest-path Cargo.toml $(CARGO_RELEASE_ARGS)
	@echo "Build complete: target/$(CARGO_RELEASE)/fs_test"

clean:
	$(CARGO) clean --manifest-path Cargo.toml

help:
	@echo "fs_test - Atom OS Filesystem Integration Test"
	@echo ""
	@echo "Targets:"
	@echo "  make build   - Compile fs_test (default)"
	@echo "  make clean   - Remove build artifacts"
	@echo "  make help    - Show this help message"
