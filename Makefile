# Atom OS — canonical pipeline entry points.
# See docs/security_pipeline.md for the full reference.

.PHONY: all ci-build ci-security ci-qemu kernel image clippy fmt fmt-check \
        audit deny geiger syscall-policy security-todos semgrep clean help

all: ci-build

## ci-build: build every critical crate + bootable image
ci-build:
	./scripts/ci/build-all.sh

## ci-security: run all mandatory security checks (no continue-on-error)
ci-security:
	./scripts/ci/security-checks.sh

## ci-qemu: adversarial QEMU smoke (SMP 1/2/4); fails without SECURITY_SMOKE PASS all
ci-qemu:
	./scripts/ci/qemu-smoke.sh

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

## audit: advisory database scan
audit:
	cargo audit

## deny: licenses / bans / sources / advisories
deny:
	cargo deny check

## geiger: unsafe baseline gate
geiger:
	./scripts/ci/geiger-baseline.sh

## syscall-policy: fail on unclassified syscall
syscall-policy:
	./scripts/ci/check-syscall-policy.sh

## security-todos: fail on untracked security debt
security-todos:
	./scripts/ci/check-security-todos.sh

## semgrep: static analysis (syscall gating)
semgrep:
	semgrep --config .semgrep/ --error

## clean: remove build artifacts
clean:
	./clean.sh

help:
	@grep -E '^##' $(MAKEFILE_LIST) | sed 's/## //'
