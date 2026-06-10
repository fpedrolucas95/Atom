//! Boot-time security invariant self-tests.
//!
//! The kernel library builds with `test = false` (its `#[cfg(test)]` modules
//! have no host harness), so the security-critical invariants are *also*
//! asserted here at boot, where the QEMU CI smoke gate executes them on every
//! push (any panic trips the gate's forbidden markers). Host-runnable suites
//! (ATXF signature + loader conformance) live in `shared/atxf` and run in the
//! CI `host-tests` job; this module covers what genuinely needs a live kernel:
//! the syscall policy table, the entropy seeding policy and the
//! product-key-bound executable authentication path.

use crate::executable::{self, ExecError};
use crate::log_info;
use crate::random::{self, entropy_policy, EntropyPolicyError, EntropyReport};

const LOG_ORIGIN: &str = "security_selftests";

pub fn run_security_self_tests() {
    log_info!(LOG_ORIGIN, "Running security invariant self-tests...");

    entropy_policy_invariants();
    aslr_base_invariants();
    atxf_authentication_invariants();
    crate::syscall::run_security_policy_selftests();

    log_info!(
        LOG_ORIGIN,
        "SECURITY_SELFTESTS PASS (entropy policy / ASLR / ATXF auth / syscall policy)"
    );
}

/// The seeding policy must accept any combination with at least one hardware
/// source and refuse jitter-only seeding (fail-closed).
fn entropy_policy_invariants() {
    let accept = [(true, true), (true, false), (false, true)];
    for (rdrand, rdseed) in accept {
        let report = EntropyReport {
            rdrand,
            rdseed,
            jitter_samples: 256,
        };
        assert!(
            entropy_policy(&report).is_ok(),
            "entropy policy must accept hardware-backed report {:?}",
            report
        );
    }

    let jitter_only = EntropyReport {
        rdrand: false,
        rdseed: false,
        jitter_samples: 256,
    };
    assert_eq!(
        entropy_policy(&jitter_only),
        Err(EntropyPolicyError::NoHardwareSource),
        "entropy policy must refuse jitter-only seeding"
    );
}

/// ASLR bases must be properly aligned, inside the user window, and must not
/// repeat back-to-back (trivial output sanity, not a statistical test).
fn aslr_base_invariants() {
    let span = 4 * 1024 * 1024;
    let first = random::random_user_base(span).expect("ASLR base unavailable");
    let second = random::random_user_base(span).expect("ASLR base unavailable");
    for base in [first, second] {
        assert_eq!(
            base % (2 * 1024 * 1024),
            0,
            "ASLR base must be 2MiB-aligned"
        );
        assert!(base >= 0x0100_0000, "ASLR base below user window");
    }
    assert_ne!(first, second, "consecutive ASLR bases must differ");

    let a = random::kernel_random_u64();
    let b = random::kernel_random_u64();
    assert_ne!(a, b, "CSPRNG returned identical consecutive u64 values");
}

/// The real product verification path must reject unsigned and tampered
/// images. (The accept path is exercised implicitly: every service the system
/// boots is authenticated through it, and boot failure fails the smoke gate.)
fn atxf_authentication_invariants() {
    // Garbage image: must never authenticate.
    let garbage = [0xA5u8; 256];
    assert!(
        executable::parse_image(&garbage).is_err(),
        "garbage image must not parse"
    );

    // Correct magic/version but zeroed (forged) signature: must be rejected by
    // the signature check specifically — proving the Ed25519 gate is wired in.
    let mut forged = alloc::vec![0u8; 4096 * 2];
    let len = forged.len() as u64;
    forged[0..4].copy_from_slice(&atom_atxf::ATXF_MAGIC.to_le_bytes());
    forged[4..6].copy_from_slice(&atom_atxf::ATXF_VERSION.to_le_bytes());
    forged[6..8].copy_from_slice(&(atom_atxf::HEADER_SIZE as u16).to_le_bytes());
    forged[8..12].copy_from_slice(&atom_atxf::ATXF_FLAG_PIE.to_le_bytes());
    // segment_count=1, relocation_count=0
    forged[20..24].copy_from_slice(&1u32.to_le_bytes());
    forged[28..36].copy_from_slice(&(atom_atxf::HEADER_SIZE as u64).to_le_bytes());
    forged[36..44].copy_from_slice(&(atom_atxf::HEADER_SIZE as u64 + 64).to_le_bytes());
    let signature_offset = 4096u64;
    forged[44..52].copy_from_slice(&signature_offset.to_le_bytes());
    forged[52..56].copy_from_slice(&(atom_atxf::ATXF_SIGNATURE_SIZE as u32).to_le_bytes());
    forged[60..68].copy_from_slice(&len.to_le_bytes());
    assert_eq!(
        executable::parse_image(&forged).unwrap_err(),
        ExecError::InvalidSignature,
        "forged signature must be rejected by the Ed25519 gate"
    );
}
