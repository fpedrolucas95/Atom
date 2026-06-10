# Contributing to Atom OS

Thanks for contributing! Atom OS is a security-focused microkernel OS, so the
contribution rules center on not regressing the security model.

## Before you push

Run the canonical pipeline locally. There is one documented way to do each
thing (see [`docs/security_pipeline.md`](docs/security_pipeline.md)):

```bash
make ci-build       # build every critical crate + bootable image
make ci-security    # fmt, clippy, syscall-policy, security-TODO, unsafe baseline,
                    # cargo audit, cargo deny, semgrep
make ci-qemu        # adversarial QEMU smoke (SMP 1/2/4)
```

or call the scripts directly:

```bash
./scripts/ci/build-all.sh
./scripts/ci/security-checks.sh
./scripts/ci/qemu-smoke.sh
```

CI (`.github/workflows/ci.yml`, `.github/workflows/security.yml`) runs the same
checks. None of them may use `continue-on-error`.

## Rules that CI enforces

1. **Every syscall is classified.** Adding `SYS_*` requires: ABI constant,
   dispatcher arm, explicit classification in `kernel/src/syscall/policy.rs`,
   a positive test, a negative `EPERM` test if it touches an external resource,
   and a justifying comment for `ExplicitlyUnrestricted`.
   `scripts/ci/check-syscall-policy.sh` fails otherwise.
2. **No untracked security debt.** `TODO security/auth/cap/unsafe`,
   `FIXME security`, `temporary allow`, `debug bypass`, `allow unsigned`,
   `fallback insecure`, `privileged process` must carry a tracking reference
   (`#123` / `ATOM-123` / `SECURITY_DEBT`) or be recorded in
   [`SECURITY_DEBT.md`](SECURITY_DEBT.md) + `.security-debt-allowlist`.
3. **No silent `unsafe` growth.** Increases above
   `scripts/ci/unsafe_baseline.txt` fail until the baseline is raised with a
   justification in `SECURITY_DEBT.md`.
4. **Critical crates stay in the pipeline.** If you add a boot/security crate,
   add it to `scripts/ci/lib.sh` so `build-all.sh` builds it.
5. **No insecure fallbacks.** Do not reintroduce unsigned/v1 ATXF acceptance,
   ambient capability grants, or dynamic port fallbacks. Fail closed.

## Style

* `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`
  must be clean.
* Match the surrounding code's conventions and comment density.
