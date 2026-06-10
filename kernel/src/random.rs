//! Kernel CSPRNG (ChaCha20) seeded from multiple independent entropy sources.
//!
//! ## Seeding policy (fail-closed)
//!
//! The seed mixes every source that is available at boot:
//!
//! * **RDRAND** — DRBG output, retried per architectural guidance.
//! * **RDSEED** — direct conditioned entropy, when the CPU supports it.
//! * **TSC jitter** — inter-sample deltas of the time-stamp counter across
//!   serializing instructions. Always available; treated as *defense in
//!   depth*, never as the sole source.
//!
//! All collected material is compressed with SHA-512 into the ChaCha20
//! key+nonce, so a failure (or silent bias) of any single source cannot
//! reduce the seed below the strength of the remaining ones. The policy
//! itself is the pure function [`entropy_policy`]: at least one *hardware*
//! source (RDRAND or RDSEED) must have produced data, otherwise `init`
//! refuses to come up. The policy and the refusal path are exercised by the
//! boot self-tests (`security_selftests`), which run under the QEMU CI gate.

use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::sync::atomic::{AtomicU64, Ordering};
use sha2::{Digest, Sha512};
use spin::Mutex;

use crate::{log_info, log_panic};

const LOG_ORIGIN: &str = "random";
const ASLR_START: usize = 0x0000_0000_0100_0000;
const ASLR_END: usize = 0x0000_0008_0000_0000;
const ASLR_ALIGNMENT: usize = 2 * 1024 * 1024;

/// 64-bit words requested from each hardware source during seeding.
const HW_SEED_WORDS: usize = 8;
/// TSC jitter samples mixed into the seed.
const JITTER_SAMPLES: usize = 256;

static RNG: Mutex<Option<ChaCha20>> = Mutex::new(None);
static LAST_USER_BASE: AtomicU64 = AtomicU64::new(0);

/// Boot-time entropy source availability, as observed by `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyReport {
    pub rdrand: bool,
    pub rdseed: bool,
    pub jitter_samples: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntropyPolicyError {
    /// No hardware entropy source produced data; jitter alone is not enough.
    NoHardwareSource,
}

/// Pure seeding policy: require at least one hardware source. Kept free of
/// I/O so the boot self-tests can exercise both the accept and the refuse
/// paths with simulated availability.
pub fn entropy_policy(report: &EntropyReport) -> Result<(), EntropyPolicyError> {
    if report.rdrand || report.rdseed {
        Ok(())
    } else {
        Err(EntropyPolicyError::NoHardwareSource)
    }
}

pub fn init() {
    let mut hasher = Sha512::new();
    hasher.update(b"AtomOS-kernel-entropy-v2");

    let rdrand = mix_rdrand(&mut hasher);
    let rdseed = mix_rdseed(&mut hasher);
    let jitter_samples = mix_tsc_jitter(&mut hasher);
    let report = EntropyReport {
        rdrand,
        rdseed,
        jitter_samples,
    };

    if let Err(error) = entropy_policy(&report) {
        log_panic!(
            LOG_ORIGIN,
            "Insufficient boot entropy ({:?}): rdrand={} rdseed={} jitter_samples={}",
            error,
            report.rdrand,
            report.rdseed,
            report.jitter_samples
        );
        panic!("strong entropy unavailable");
    }

    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest[..32]);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&digest[32..44]);
    *RNG.lock() = Some(ChaCha20::new(key, nonce));

    log_info!(
        LOG_ORIGIN,
        "CSPRNG initialized (ChaCha20, SHA-512 mix): rdrand={} rdseed={} jitter_samples={}",
        report.rdrand,
        report.rdseed,
        report.jitter_samples
    );
}

/// Feed RDRAND output into the seed hash. Returns whether the source is
/// present *and* actually produced all requested words.
fn mix_rdrand(hasher: &mut Sha512) -> bool {
    let features = __cpuid(1);
    if features.ecx & (1 << 30) == 0 {
        return false;
    }
    for _ in 0..HW_SEED_WORDS {
        match rdrand_u64() {
            Some(value) => hasher.update(value.to_le_bytes()),
            None => return false,
        }
    }
    true
}

/// Feed RDSEED output into the seed hash. RDSEED can underflow legitimately
/// (it surfaces raw conditioned entropy), so exhaustion after retries simply
/// marks the source unavailable instead of failing the boot.
fn mix_rdseed(hasher: &mut Sha512) -> bool {
    let features = __cpuid_count(7, 0);
    if features.ebx & (1 << 18) == 0 {
        return false;
    }
    for _ in 0..HW_SEED_WORDS {
        match rdseed_u64() {
            Some(value) => hasher.update(value.to_le_bytes()),
            None => return false,
        }
    }
    true
}

/// Feed TSC jitter deltas into the seed hash. CPUID serializes execution
/// between samples so each delta carries pipeline/cache timing noise.
fn mix_tsc_jitter(hasher: &mut Sha512) -> u32 {
    let mut previous = read_tsc();
    for _ in 0..JITTER_SAMPLES {
        __cpuid(0);
        let now = read_tsc();
        hasher.update(now.wrapping_sub(previous).to_le_bytes());
        previous = now;
    }
    hasher.update(previous.to_le_bytes());
    JITTER_SAMPLES as u32
}

pub fn kernel_random_fill(output: &mut [u8]) {
    let mut guard = RNG.lock();
    let rng = guard.as_mut().expect("CSPRNG used before initialization");
    rng.fill(output);
}

pub fn kernel_random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    kernel_random_fill(&mut bytes);
    u64::from_le_bytes(bytes)
}

pub fn random_user_base(image_span: usize) -> Option<usize> {
    let span = align_up(image_span, ASLR_ALIGNMENT)?;
    let high = ASLR_END.checked_sub(span)?;
    if high <= ASLR_START {
        return None;
    }
    let slots = (high - ASLR_START) / ASLR_ALIGNMENT;
    if slots == 0 {
        return None;
    }

    for _ in 0..8 {
        let slot = (kernel_random_u64() as usize) % slots;
        let base = ASLR_START.checked_add(slot.checked_mul(ASLR_ALIGNMENT)?)?;
        let previous = LAST_USER_BASE.swap(base as u64, Ordering::SeqCst) as usize;
        if base != previous {
            return Some(base);
        }
    }
    None
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

fn rdrand_u64() -> Option<u64> {
    for _ in 0..16 {
        let value: u64;
        let success: u8;
        unsafe {
            asm!(
                "rdrand {value}",
                "setc {success}",
                value = out(reg) value,
                success = out(reg_byte) success,
                options(nostack, nomem)
            );
        }
        if success != 0 {
            return Some(value);
        }
    }
    None
}

fn rdseed_u64() -> Option<u64> {
    // RDSEED underflows more often than RDRAND; give it more attempts with a
    // pause between them, per Intel DRNG guidance.
    for _ in 0..64 {
        let value: u64;
        let success: u8;
        unsafe {
            asm!(
                "rdseed {value}",
                "setc {success}",
                value = out(reg) value,
                success = out(reg_byte) success,
                options(nostack, nomem)
            );
        }
        if success != 0 {
            return Some(value);
        }
        core::hint::spin_loop();
    }
    None
}

fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nostack, nomem)
        );
    }
    ((high as u64) << 32) | low as u64
}

struct ChaCha20 {
    state: [u32; 16],
    block: [u8; 64],
    available: usize,
}

impl ChaCha20 {
    fn new(key: [u8; 32], nonce: [u8; 12]) -> Self {
        let mut state = [0u32; 16];
        state[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
        for (index, chunk) in key.chunks_exact(4).enumerate() {
            state[4 + index] = u32::from_le_bytes(chunk.try_into().unwrap());
        }
        state[12] = 0;
        for (index, chunk) in nonce.chunks_exact(4).enumerate() {
            state[13 + index] = u32::from_le_bytes(chunk.try_into().unwrap());
        }
        Self {
            state,
            block: [0; 64],
            available: 0,
        }
    }

    fn fill(&mut self, output: &mut [u8]) {
        let mut written = 0;
        while written < output.len() {
            if self.available == 0 {
                self.refill();
            }
            let offset = 64 - self.available;
            let take = self.available.min(output.len() - written);
            output[written..written + take].copy_from_slice(&self.block[offset..offset + take]);
            self.available -= take;
            written += take;
        }
    }

    fn refill(&mut self) {
        let mut working = self.state;
        for _ in 0..10 {
            quarter_round(&mut working, 0, 4, 8, 12);
            quarter_round(&mut working, 1, 5, 9, 13);
            quarter_round(&mut working, 2, 6, 10, 14);
            quarter_round(&mut working, 3, 7, 11, 15);
            quarter_round(&mut working, 0, 5, 10, 15);
            quarter_round(&mut working, 1, 6, 11, 12);
            quarter_round(&mut working, 2, 7, 8, 13);
            quarter_round(&mut working, 3, 4, 9, 14);
        }
        for index in 0..16 {
            let word = working[index].wrapping_add(self.state[index]);
            self.block[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        self.state[12] = self.state[12].wrapping_add(1);
        if self.state[12] == 0 {
            panic!("CSPRNG counter exhausted");
        }
        self.available = 64;
    }
}

fn quarter_round(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(7);
}
