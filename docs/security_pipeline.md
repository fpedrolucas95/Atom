# Atom OS Security Pipeline

This document is the single, canonical reference for building, testing and
security-checking Atom OS. Everything here is enforced in CI
(`.github/workflows/ci.yml`, `.github/workflows/security.yml`) with **no
`continue-on-error`** — a failing gate breaks the build.

## Canonical commands

| Purpose | Command |
| --- | --- |
| Build every critical crate + bootable image | `./scripts/ci/build-all.sh` (or `make ci-build`) |
| Run all mandatory security checks | `./scripts/ci/security-checks.sh` (or `make ci-security`) |
| Run the adversarial QEMU smoke (SMP 1/2/4) | `./scripts/ci/qemu-smoke.sh` (or `make ci-qemu`) |
| Kernel only | `cargo build -p atom-kernel --release` |
| Full workspace (host UEFI target) | `cargo build --workspace --release` |
| Formatting | `cargo fmt --all --check` |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` |
| Advisories | `cargo audit` |
| Licenses / bans / sources | `cargo deny check` |
| Unsafe baseline | `./scripts/ci/geiger-baseline.sh` (+ `cargo geiger`) |
| Static analysis (syscall gating) | `semgrep --config .semgrep/ --error` |

`security-checks.sh` runs fmt, clippy, the syscall-policy gate, the
security-debt/TODO gate, the unsafe baseline, `cargo audit`, `cargo deny` and
`semgrep` in order and fails on the first problem.

## What each gate guarantees

### 1. Build (`scripts/ci/build-all.sh`, `build.sh`)
Builds the host-target workspace crates, the host tools, **every bare-metal
critical crate from its own directory** (so nothing compiles via a hidden
manual path), and finally the full bootable image. The list of critical crates
lives in `scripts/ci/lib.sh` and is the source of truth for the pipeline.

### 2. Syscall policy gate (`scripts/ci/check-syscall-policy.sh`)
Diffs the set of `SYS_*` constants in the ABI
(`userspace/libs/syscall/src/raw.rs`) against the classifications in
`kernel/src/syscall/policy.rs`. A new syscall fails CI until it is **explicitly**
classified — relying on the fail-closed wildcard is not allowed. Also asserts
the fail-closed wildcard is still present.

Every new syscall must ship with:
1. a `SYS_*` ABI constant,
2. a dispatcher arm in `kernel/src/syscall/mod.rs`,
3. an explicit `syscall_policy` classification,
4. a positive authorization test,
5. a negative `EPERM` test if it touches an external resource,
6. a comment justifying `ExplicitlyUnrestricted`, where used.

The Semgrep rules in `.semgrep/syscall-policy.yml` add a static backstop:
`ExplicitlyUnrestricted` without a justifying comment, sensitive handlers
missing a capability/ownership check, and any reintroduced unsigned/v1 ATXF
fallback are flagged as errors.

### 3. Security-debt / TODO gate (`scripts/ci/check-security-todos.sh`)
Fails on untracked security debt: `TODO security|auth|cap|unsafe`,
`FIXME security`, `temporary allow`, `debug bypass`, `allow unsigned`,
`fallback insecure`, and a word-boundary `privileged process`. An occurrence is
allowed only if it carries a tracking reference (`#123`, `ATOM-123`, or the
token `SECURITY_DEBT`) **or** is listed in `.security-debt-allowlist` (a
reviewed file; the git diff is the audit trail). Accepted, long-lived
exceptions are documented in [`SECURITY_DEBT.md`](../SECURITY_DEBT.md).

### 4. Unsafe baseline (`scripts/ci/geiger-baseline.sh`)
Counts `unsafe` per critical crate and fails if it **exceeds** the committed
baseline in `scripts/ci/unsafe_baseline.txt`. Decreases are always fine.
Raising a baseline number is a reviewed change and must be justified in
`SECURITY_DEBT.md`. This makes silent growth of `unsafe` in the TCB impossible.
When `cargo-geiger` is installed the full report is produced as a CI artifact.

### 5. Supply chain (`deny.toml`, `.cargo/audit.toml`)
`cargo audit` fails on any relevant advisory; `cargo deny check` enforces the
license allowlist, flags duplicate versions, bans wildcard dependencies, and
rejects unknown registries/git sources.

### 6. QEMU adversarial smoke (`scripts/ci/qemu-smoke.sh`)
Builds a *smoke* image (`SMOKE_BUILD=1 ./build.sh`, which enables init's
`smoke` feature so `app_launcher` auto-launches the unprivileged
`security_smoke` app), boots it headless for SMP=1, 2 and 4, and parses the
serial logs. It FAILS unless every boot prints `SECURITY_SMOKE PASS all` and
contains no `SECURITY_SMOKE FAIL`, `PANIC`, or `Fatal kernel page fault`.

## `security_smoke` coverage

`security_smoke` is launched by path as an ordinary, zero-capability app and
asserts that every sensitive operation is denied. It prints one
machine-readable line per category and a final aggregate verdict:

```
SECURITY_SMOKE PASS spawn_denied          # PR1: SYS_SPAWN_PROCESS/FROM_PATH/READ_KLOG -> EPERM
SECURITY_SMOKE PASS reserved_port_denied  # PR2: create Port(1/2/3) -> EPERM
SECURITY_SMOKE PASS namesvc_spoof_denied  # PR2: cannot squat namesvc port(2)
SECURITY_SMOKE PASS framebuffer_denied    # PR3: framebuffer / video-mode -> EPERM
SECURITY_SMOKE PASS input_denied          # PR3: keyboard / mouse -> EPERM
SECURITY_SMOKE PASS hardware_denied       # PR3: kernel FS backend + PCI/DMA denied
SECURITY_SMOKE PASS rwx_mmap_denied       # PR4: mmap(RWX) -> EPERM
SECURITY_SMOKE PASS rwx_mprotect_denied   # PR4: mprotect(RW->RWX) -> EPERM
SECURITY_SMOKE PASS shared_exec_denied    # PR4: shared EXEC + map_region(W|X) -> EPERM
SECURITY_SMOKE PASS fsd_limits            # PR5: oversized fsd request -> controlled error
SECURITY_SMOKE PASS all
```

The runner keys off `SECURITY_SMOKE PASS all`; a regression turns the relevant
line into `SECURITY_SMOKE FAIL <tag>` and suppresses `PASS all`, failing CI.

## Adversarial test matrix (PR1–PR5)

| Milestone | Scenario | Where enforced |
| --- | --- | --- |
| PR1 | `SYS_SPAWN_PROCESS` / `SYS_SPAWN_FROM_PATH` / `SYS_READ_KLOG` from app → EPERM | smoke `spawn_denied` + policy tests |
| PR1 | fork does not inherit Spawn*/ReadKernelLog caps | kernel manifest/cap tests |
| PR2 | create Port(1/2/3) from app → EPERM | smoke `reserved_port_denied` |
| PR2 | register without ServiceIdentity / spoofed name / overwrite live / unregister others → denied | namesvc unit tests; smoke `namesvc_spoof_denied` (port-squat proxy) |
| PR3 | framebuffer / input / mode-set / PCI/MMIO/IRQ/DMA / kernel FS backend → EPERM | smoke `framebuffer_denied`/`input_denied`/`hardware_denied` |
| PR4 | ATXF v1 / unsigned / tampered rejected; RWX mmap/mprotect; shared EXEC; map_region W+X; per-run ASLR; no W+X PTE | loader tests; smoke `rwx_*`/`shared_exec_denied`; `no-unsigned-atxf-fallback` semgrep rule |
| PR5 | oversized path/read/write → controlled error; flood has no monotonic leak; transient OOM → ENOMEM; fatal OOM exits → service_manager restarts → fsd re-registers Port(3); clients never block | smoke `fsd_limits`; fsd allocator + limits; service_manager restart; namesvc re-register |
| Boot | SMP 1/2/4 boot, no PANIC, no Fatal kernel page fault, no insecure fallback | qemu-smoke runner |

## fsd availability (PR5)

* **Allocator** — `userspace/services/fsd/src/allocator.rs` replaces the
  irreversible bump allocator with a segregated free-list allocator carved from
  a single `mmap`-backed, RW-only (never EXEC, preserving W^X) region. Freed
  blocks are reused per size class; oversized/over-aligned allocations get their
  own `mmap`/`munmap`. OOM returns null (controlled), the alloc-error handler
  exits cleanly for restart, and repeated requests no longer grow memory
  monotonically.
* **Limits** — `userspace/services/fsd/src/limits.rs` bounds path length,
  request payload, read/write size, file-image size, open handles, pending
  requests, directory entries, mount points and cache buffers. Client-supplied
  size fields are validated/clamped before any allocation.
* **OOM / restart** — fatal/OOM paths call `thread::exit` (no spin loop), so
  `service_manager` observes the death and restarts fsd, which re-claims the
  reserved FS port and re-registers with `namesvc` (allowed once the previous
  owner is confirmed dead).
