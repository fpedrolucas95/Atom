# Development signing keys

`atxf_signing.seed` is the **development** Ed25519 private seed (hex) used by
host tooling (`elf2atxf`) to sign ATXF v3 executables. The matching public key
is embedded in the kernel as `atom_atxf::ATXF_DEV_VERIFYING_KEY`.

This keypair is intentionally committed so that anyone can build and boot a
development image. It provides **no production security**: production builds
must generate their own keypair, keep the seed in protected release
infrastructure (set `ATXF_SIGNING_KEY_FILE` for elf2atxf), and replace the
embedded verifying key.
