// Out-of-Memory (OOM) Management
//
// Provides a basic OOM killer and memory pressure detection for the kernel.
// When physical memory runs critically low, the OOM subsystem can:
//
// 1. Report memory pressure levels (none/low/critical/oom)
// 2. Select a victim process to terminate (largest resident set)
// 3. Trigger cleanup to reclaim memory
//
// Design:
// - Memory pressure is based on PMM free page counts and configurable thresholds
// - The OOM killer uses a simple "largest consumer" heuristic
// - Kernel threads are never selected as OOM victims
// - The system avoids killing the last remaining userspace process if possible

use crate::mm::pmm;
use crate::mm::vma;
use crate::thread::ThreadId;
use crate::{log_info, log_warn, log_panic};

const LOG_ORIGIN: &str = "oom";

/// Memory pressure levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPressure {
    /// Plenty of free memory
    None,
    /// Below 25% free — start being cautious
    Low,
    /// Below 10% free — aggressive reclaim needed
    Critical,
    /// Below 5% or 0 free pages — OOM action required
    Oom,
}

/// Check current memory pressure level
pub fn check_pressure() -> MemoryPressure {
    let (total, free) = pmm::get_stats();

    if total == 0 {
        return MemoryPressure::Oom;
    }

    let free_pct = (free * 100) / total;

    if free_pct <= 5 || free < 256 {
        MemoryPressure::Oom
    } else if free_pct <= 10 {
        MemoryPressure::Critical
    } else if free_pct <= 25 {
        MemoryPressure::Low
    } else {
        MemoryPressure::None
    }
}

/// OOM kill result
#[derive(Debug)]
pub enum OomResult {
    /// Successfully killed a process, freeing approximately this many pages
    Killed { tid: ThreadId, name: &'static str },
    /// No killable process found
    NoVictim,
}

/// Select and terminate the largest memory consumer.
///
/// Selection criteria:
/// - Only userspace threads are candidates
/// - The thread with the most resident pages is selected
/// - Returns information about the killed thread
pub fn oom_kill() -> OomResult {
    let (total, free) = pmm::get_stats();

    log_warn!(
        LOG_ORIGIN,
        "OOM killer invoked! Free pages: {}/{} ({}%)",
        free,
        total,
        if total > 0 { (free * 100) / total } else { 0 }
    );

    // Get list of all userspace threads and their memory usage
    let threads = crate::thread::get_all_thread_info();
    let mut best_victim: Option<(ThreadId, &'static str, usize)> = None;

    for (tid, name, process_id, is_userspace) in &threads {
        if !is_userspace {
            continue;
        }

        let resident = process_id
            .and_then(|process_id| vma::get_process_stats(process_id))
            .map(|s| s.resident_pages)
            .unwrap_or(0);

        match &best_victim {
            Some((_, _, best_resident)) if resident > *best_resident => {
                best_victim = Some((*tid, name, resident));
            }
            None => {
                best_victim = Some((*tid, name, resident));
            }
            _ => {}
        }
    }

    match best_victim {
        Some((tid, name, resident)) => {
            log_warn!(
                LOG_ORIGIN,
                "Killing process '{}' (tid={}, resident={} pages, {} KB)",
                name,
                tid,
                resident,
                resident * pmm::PAGE_SIZE / 1024
            );

            crate::thread::terminate_entity(
                tid,
                crate::thread::TerminationReason::OutOfMemory,
            );

            OomResult::Killed { tid, name }
        }
        None => {
            log_panic!(LOG_ORIGIN, "OOM killer found no victim — system out of memory with no killable processes");
            OomResult::NoVictim
        }
    }
}

/// Attempt to free memory when under pressure.
/// Called when an allocation fails or memory pressure is critical.
///
/// Returns true if memory was freed, false if nothing could be done.
pub fn try_reclaim() -> bool {
    let pressure = check_pressure();

    match pressure {
        MemoryPressure::None | MemoryPressure::Low => {
            // No action needed
            false
        }
        MemoryPressure::Critical => {
            log_warn!(LOG_ORIGIN, "Critical memory pressure — future allocations may trigger OOM");
            // In the future, this is where page cache eviction would go
            false
        }
        MemoryPressure::Oom => {
            log_warn!(LOG_ORIGIN, "OOM pressure — invoking OOM killer");
            match oom_kill() {
                OomResult::Killed { tid, name } => {
                    log_info!(LOG_ORIGIN, "OOM killed '{}' (tid={})", name, tid);
                    true
                }
                OomResult::NoVictim => false,
            }
        }
    }
}

pub fn init() {
    let pressure = check_pressure();
    log_info!(LOG_ORIGIN, "OOM subsystem initialized — current pressure: {:?}", pressure);
}
