# Security Policy — Atom OS

Atom OS is a from-scratch capability-based microkernel OS. Security is enforced
by construction and protected against regression by a mandatory CI pipeline.

## Security model (PR1–PR5)

* **PR1 — Kernel-side root of trust.** No "privileged process" concept.
  Authority to spawn processes or read the kernel log comes only from the
  kernel-side `SystemServiceManifest`; spawn is capability-gated.
* **PR2 — Authenticated IPC.** Reserved bootstrap ports, `ServiceIdentity`,
  kernel-generated IPC envelopes, and a fail-closed `namesvc`.
* **PR3 — Least privilege.** Per-service capability profiles; no ambient
  grants; sensitive syscalls gated by specific capabilities.
* **PR4 — Trusted loading.** Mandatory ATXF v2, authenticity verified before
  mapping, PIE/ASLR, a single loader, and W^X everywhere.
* **PR5 — Controlled availability + pipeline.** A reusable, `mmap`-backed fsd
  allocator, per-request limits, recoverable OOM/restart, and a mandatory
  security pipeline that blocks regressions.

## Reporting a vulnerability

Open a private security advisory on the repository, or email the maintainer.
Please do not file public issues for undisclosed vulnerabilities.

## Running the security checks

All commands are documented in
[`docs/security_pipeline.md`](docs/security_pipeline.md). The short version:

```bash
make ci-build       # build every critical crate + bootable image
make ci-security    # fmt, clippy, syscall-policy, TODO gate, unsafe baseline,
                    # cargo audit, cargo deny, semgrep
make ci-qemu        # adversarial QEMU smoke (SMP 1/2/4); fails unless
                    # `SECURITY_SMOKE PASS all`
```

These are enforced in `.github/workflows/ci.yml` and
`.github/workflows/security.yml` with **no `continue-on-error`**.

## Adding a syscall

A new syscall must have: an ABI constant, a dispatcher arm, an explicit
classification in `kernel/src/syscall/policy.rs`, a positive authorization
test, a negative `EPERM` test if it touches an external resource, and a comment
justifying `ExplicitlyUnrestricted` where used. `scripts/ci/check-syscall-policy.sh`
fails CI for any unclassified syscall.

## Security exceptions

Exceptions live only in [`SECURITY_DEBT.md`](SECURITY_DEBT.md) (and, for code
mentions, `.security-debt-allowlist`). A loose `TODO`/`FIXME` with security
keywords and no tracking reference fails CI.

## Unsafe code

`unsafe` in the TCB is bounded by `scripts/ci/unsafe_baseline.txt`. Any increase
fails CI until the baseline is raised with a justification in `SECURITY_DEBT.md`.
