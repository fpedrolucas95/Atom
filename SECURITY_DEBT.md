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

### SD-003 — Legacy tree not yet `cargo fmt` / `clippy -D warnings` clean
* **Area:** workspace members (`kernel`, `shared/*`, `userspace/libs/*`).
* **What:** The pre-existing code predates the fmt/clippy gates and is not
  rustfmt-clean or warning-free under `-D warnings`. A one-time normalization
  would touch ~90 files and was deliberately **not** bundled into this
  security PR, to keep the security diff reviewable (reformatting `cap.rs`,
  `shared_mem.rs`, etc. wholesale would bury the actual changes).
* **Containment:** fmt/clippy run in their own CI job (`format-and-lint`),
  separate from the security gates (`security-gates`), so a style nit can never
  block or mask a security check. This PR's own changed files are fmt-clean.
* **Exit criteria:** A dedicated normalization PR runs `cargo fmt --all` and
  resolves the clippy warnings, after which `format-and-lint` goes green.

---

## Accepted, non-debt exceptions

The `"privileged process"` mentions allowlisted in `.security-debt-allowlist`
are documentation comments that **reject** the removed privileged-process
concept (PR1–PR3). They are not debt and require no exit criteria.

## Resolved

### SD-R001 — `SYS_IO_PORT_READ/WRITE` relied on the fail-closed wildcard
Fixed in this PR: both are now classified `ExplicitlyUnrestricted` in
`kernel/src/syscall/policy.rs` with a comment noting they are gated in-handler
by `validate_io_port_access` (IoPort/Device capability). Previously they fell
through to the wildcard, making the handler unreachable. Caught by the new
`scripts/ci/check-syscall-policy.sh` gate.
