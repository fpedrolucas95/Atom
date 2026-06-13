# Atom OS — build entry points.

.PHONY: all kernel image clippy fmt fmt-check clean help

all: image

## kernel: build the kernel only
kernel:
	cargo build -p atom-kernel --release

## image: full bootable image
image:
	./build.sh

## fmt: format the workspace
fmt:
	cargo fmt --all

## fmt-check: verify formatting
fmt-check:
	cargo fmt --all --check

## clippy: lint with warnings denied
# --lib --bins (not --all-targets): the default target is the no_std
# x86_64-unknown-uefi target, whose --tests/--benches targets cannot link the
# `test` crate (E0463), so --all-targets can never succeed here.
clippy:
	cargo clippy --workspace --lib --bins -- -D warnings

## clean: remove build artifacts
clean:
	./clean.sh

help:
	@grep -E '^##' $(MAKEFILE_LIST) | sed 's/## //'
