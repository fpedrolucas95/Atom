# Atom OS — Security Debt Register

This file is the **only** approved home for security exceptions that cannot yet
be fixed. The CI gates (`scripts/ci/check-security-todos.sh`,
`scripts/ci/geiger-baseline.sh`) refuse untracked debt: an exception is valid
only if it is recorded here and (where applicable) referenced from the code via
a tracking token or listed in `.security-debt-allowlist`.

Adding an entry here is a deliberate, reviewed act. Prefer fixing the debt.

## Format

Each entry: a stable id, the affected area, why it exists, the containment
(what stops it becoming a real hole today), and the exit criteria.

---

## Open debt

### SD-001 — Critical bare-metal crates not in a single Cargo workspace/lockfile
* **Area:** `Cargo.toml`, the bare-metal service/app crates.
* **What:** `kernel`, `shared/*` and the host-buildable libs share the root
  workspace and lockfile. The bare-metal crates (`init`, `namesvc`,
  `service_manager`, `fsd`, `app_launcher`, `netd`, `nic_driver`, the
  `system_apps/*`, `apps/security_smoke`) build for `x86_64-unknown-none` with
  per-crate `.cargo/config.toml` (PIE linker + `build-std`) and therefore keep
  their own lockfiles. A single `cargo build --workspace` cannot unify uefi /
  none / host targets in one invocation.
* **Containment:** Every critical crate is enumerated in `scripts/ci/lib.sh`
  and built by `scripts/ci/build-all.sh`, so none compiles via a hidden manual
  path — they all appear in the pipeline and break CI if they stop compiling.
  The unsafe baseline and security gates cover them by path, not by workspace
  membership.
* **Exit criteria:** Introduce a per-target workspace layout (or
  `build-std`-aware unified workspace) that lets a single command resolve one
  lockfile across all targets without regressing the PIE/W^X link flags, then
  fold the bare-metal crates into `members` and delete their standalone
  lockfiles.

### SD-002 — QEMU autostart of `security_smoke` is unverified in the offline dev container
* **Area:** `userspace/services/init` (`smoke` feature), `scripts/ci/qemu-smoke.sh`.
* **What:** The smoke autostart path (init → `app_launcher` →
  `SYS_SPAWN_FROM_PATH`) and the headless QEMU run were authored and
  compile-checked, but could not be booted end-to-end in the development
  container (no QEMU/OVMF here). The logic is feature-gated (`--features smoke`,
  off by default) so normal boots are unaffected.
* **Containment:** `.github/workflows/ci.yml` runs `qemu-smoke.sh` on a runner
  with QEMU+OVMF and fails unless `SECURITY_SMOKE PASS all` is observed for
  SMP 1/2/4. The gate is a hard failure, not `continue-on-error`.
* **Exit criteria:** First green `qemu-smoke` job on CI; then this entry is
  closed.

---

## Accepted, non-debt exceptions

The `"privileged process"` mentions allowlisted in `.security-debt-allowlist`
are documentation comments that **reject** the removed privileged-process
concept (PR1–PR3). They are not debt and require no exit criteria.

## Resolved

### SD-R003 — Legacy tree not yet `cargo fmt` / `clippy -D warnings` clean
Resolved by the documentation/CI normalization pass: `cargo fmt --all` was run
across the workspace and the `cargo clippy -- -D warnings` findings were fixed
(idiomatic rewrites where safe, narrowly-scoped `#[allow(...)]` with a rationale
where a refactor would change a deliberate signature). The `format-and-lint`
job now goes green. The clippy gate command was also corrected from
`--all-targets` to `--lib --bins`: the default build target is the `no_std`
`x86_64-unknown-uefi` target, for which the `--tests`/`--benches` targets cannot
link the `test` crate (`E0463`), so `--all-targets` could never succeed.

### SD-R002 — Unsafe baseline re-synced after the ATXF v3 / compositor merge
The audited `unsafe` token counts for `kernel` (346 → 347) and
`userspace/system_apps/ui_shell` (17 → 19) grew during the ATXF v3 Ed25519
signing / multi-source entropy work and the compositor livelock fix, but
`scripts/ci/unsafe_baseline.txt` was not updated in that change, leaving the
`geiger-baseline` gate red. The baseline has been re-synced to the current
audited values (the diff is the audit trail); no new unsafe was introduced by
this normalization pass.

### SD-R001 — `SYS_IO_PORT_READ/WRITE` relied on the fail-closed wildcard
Fixed in this PR: both are now classified `ExplicitlyUnrestricted` in
`kernel/src/syscall/policy.rs` with a comment noting they are gated in-handler
by `validate_io_port_access` (IoPort/Device capability). Previously they fell
through to the wildcard, making the handler unreachable. Caught by the new
`scripts/ci/check-syscall-policy.sh` gate.
