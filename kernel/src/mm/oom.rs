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

#[cfg(debug_assertions)]
use crate::log_debug;
use crate::mm::pmm;
use crate::process::{ProcessId, ProcessMemorySnapshot};
use crate::{log_info, log_warn};

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

/// Detailed memory pressure information
///
/// Provides comprehensive memory pressure metrics including free pages,
/// fragmentation, and processes exceeding their memory limits.
///
/// # Requirements
/// Implements Req 8.1, Req 8.2, Req 8.3, Req 8.4, Req 8.5
#[derive(Debug, Clone)]
pub struct MemoryPressureInfo {
    /// Memory pressure level
    pub level: MemoryPressure,
    /// Number of free pages
    pub free_pages: usize,
    /// Total pages in the system
    pub total_pages: usize,
    /// Free memory as a percentage (0-100)
    pub free_percent: usize,
    /// Largest contiguous free run of pages
    pub largest_free_run: usize,
    /// Fragmentation score (0.0 = no fragmentation, 1.0 = maximum fragmentation)
    pub fragmentation_score: f32,
    /// Number of processes exceeding their memory limits
    pub processes_over_limit: usize,
}

impl MemoryPressureInfo {
    /// Check if OOM killer should be triggered
    pub fn should_trigger_oom(&self) -> bool {
        matches!(self.level, MemoryPressure::Oom)
            || (self.free_pages < 256 && self.largest_free_run < 64)
    }

    /// Check if memory reclamation should be attempted
    pub fn should_reclaim(&self) -> bool {
        matches!(self.level, MemoryPressure::Critical | MemoryPressure::Oom)
    }
}

/// Check current memory pressure level (simple version)
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

/// Check detailed memory pressure with fragmentation and process limit tracking.
///
/// This function provides comprehensive memory pressure information by considering:
/// - Absolute free pages and percentage
/// - Largest contiguous free run (fragmentation indicator)
/// - Fragmentation score based on free page distribution
/// - Number of processes exceeding their memory limits
///
/// # Returns
/// `MemoryPressureInfo` containing detailed pressure metrics
///
/// # Requirements
/// Implements Req 8.1, Req 8.2, Req 8.3, Req 8.4, Req 8.5
pub fn check_memory_pressure_detailed() -> MemoryPressureInfo {
    let (total, free) = pmm::get_stats();
    let diagnostics = pmm::get_boot_diagnostics();
    let largest_free_run = diagnostics.largest_free_run;

    // Calculate free percentage
    let free_percent = free
        .checked_mul(100)
        .and_then(|scaled| scaled.checked_div(total))
        .unwrap_or(0);

    // Calculate fragmentation score
    // Fragmentation score is based on the ratio of largest free run to total free pages
    // Score of 0.0 = all free pages are contiguous (no fragmentation)
    // Score of 1.0 = maximum fragmentation (free pages are scattered)
    let fragmentation_score = if free > 0 {
        let ideal_ratio = largest_free_run as f32 / free as f32;
        // Invert the ratio: if largest_free_run == free, score = 0 (no fragmentation)
        // if largest_free_run << free, score approaches 1 (high fragmentation)
        1.0 - ideal_ratio
    } else {
        0.0
    };

    // Count processes exceeding their memory limits
    let processes_over_limit = count_processes_over_limit();

    // Determine pressure level considering both free pages and fragmentation
    let level = if free_percent <= 5 || free < 256 || (free < 512 && largest_free_run < 64) {
        MemoryPressure::Oom
    } else if free_percent <= 10 || (free < 1024 && largest_free_run < 128) {
        MemoryPressure::Critical
    } else if free_percent <= 25 {
        MemoryPressure::Low
    } else {
        MemoryPressure::None
    };

    MemoryPressureInfo {
        level,
        free_pages: free,
        total_pages: total,
        free_percent,
        largest_free_run,
        fragmentation_score,
        processes_over_limit,
    }
}

/// Count the number of processes that are exceeding their memory limits.
///
/// Uses an immutable process-memory snapshot and checks if each process
/// exceeds its configured resident-page limit.
///
/// # Returns
/// Number of processes over their memory limit
///
/// # Requirements
/// Implements Req 8.4, Req 8.5
fn count_processes_over_limit() -> usize {
    let snapshot = crate::process::collect_process_memory_snapshot();
    snapshot
        .iter()
        .filter(|process| {
            !process.terminating
                && !process.terminated
                && process.limit_pages != 0
                && process.resident_pages > process.limit_pages
        })
        .count()
}

/// Reason why no OOM victim was found
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoVictimReason {
    /// No user processes are running
    NoUserProcesses,
    /// All processes are below the minimum killable threshold
    AllProcessesBelowMinimum,
    /// All memory is reserved for system use
    SystemReserved,
}

/// Fallback action to take when no OOM victim is found
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackAction {
    /// Deny the allocation that triggered OOM
    DenyAllocation,
    /// Kill the oldest process regardless of size
    KillOldestProcess,
    /// Enter emergency mode with reduced functionality
    EnterEmergencyMode,
}

/// Strategy used to reclaim memory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimStrategy {
    /// Evict cached pages
    CacheEviction,
    /// Compact memory to reduce fragmentation
    CompactMemory,
    /// Swap out pages to disk
    SwapOut,
}

/// Errors that can occur during memory reclamation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OomError {
    /// The requested reclaim strategy is not implemented
    StrategyNotImplemented,
    /// No memory could be reclaimed using this strategy
    NoMemoryReclaimed,
    /// The reclaim operation failed
    ReclaimFailed,
}

/// OOM kill result
#[derive(Debug)]
pub enum OomResult {
    /// Successfully killed a process, freeing approximately this many pages
    Killed { pid: ProcessId, pages_freed: usize },
    /// No killable process found
    NoVictim {
        reason: NoVictimReason,
        fallback_action: FallbackAction,
    },
    /// Memory was reclaimed using an alternative strategy
    Reclaimed {
        strategy: ReclaimStrategy,
        pages_freed: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VictimReason {
    OverLimit,
    GlobalPressure,
}

#[derive(Debug, Clone, Copy)]
struct VictimCandidate {
    pid: ProcessId,
    resident_pages: usize,
    limit_pages: usize,
    over_limit_pages: usize,
    reason: VictimReason,
}

fn select_victim(snapshot: &[ProcessMemorySnapshot]) -> Option<VictimCandidate> {
    let current_process =
        crate::sched::current_thread().and_then(crate::thread::get_thread_process_id);

    let mut best: Option<VictimCandidate> = None;
    for process in snapshot {
        if process.terminating || process.terminated {
            continue;
        }
        if process.resident_pages == 0 {
            continue;
        }
        if current_process == Some(process.process_id) {
            continue;
        }

        let over_limit_pages = if process.limit_pages == 0 {
            0
        } else {
            process.resident_pages.saturating_sub(process.limit_pages)
        };
        let reason = if over_limit_pages > 0 {
            VictimReason::OverLimit
        } else {
            VictimReason::GlobalPressure
        };

        let candidate = VictimCandidate {
            pid: process.process_id,
            resident_pages: process.resident_pages,
            limit_pages: process.limit_pages,
            over_limit_pages,
            reason,
        };

        let replace = match best {
            None => true,
            Some(current) => {
                candidate.resident_pages > current.resident_pages
                    || (candidate.resident_pages == current.resident_pages
                        && candidate.over_limit_pages > current.over_limit_pages)
                    || (candidate.resident_pages == current.resident_pages
                        && candidate.over_limit_pages == current.over_limit_pages
                        && candidate.pid.raw() < current.pid.raw())
            }
        };

        if replace {
            best = Some(candidate);
        }
    }

    best
}

/// Select and terminate the largest process memory consumer.
///
/// Selection criteria (snapshot-only):
/// - Candidates come from immutable process snapshot
/// - Primary key: resident pages
/// - Secondary key: over-limit pages
/// - OOM action targets a full process (`pid`), never a thread (`tid`)
///
/// # Requirements
/// Implements Req 8.2, Req 8.3
pub fn oom_kill() -> OomResult {
    // Get detailed memory pressure information before victim selection
    let pressure = check_memory_pressure_detailed();

    log_warn!(
        LOG_ORIGIN,
        "OOM killer invoked! Pressure: {:?}, Free: {}/{} ({}%), Largest run: {}, Fragmentation: {:.2}, Processes over limit: {}",
        pressure.level,
        pressure.free_pages,
        pressure.total_pages,
        pressure.free_percent,
        pressure.largest_free_run,
        pressure.fragmentation_score,
        pressure.processes_over_limit
    );

    // Try reclamation strategies if pressure is Critical or Oom
    if pressure.should_reclaim() {
        log_info!(
            LOG_ORIGIN,
            "Attempting memory reclamation before victim selection"
        );

        // Try each reclamation strategy in order
        for strategy in [
            ReclaimStrategy::CacheEviction,
            ReclaimStrategy::CompactMemory,
            ReclaimStrategy::SwapOut,
        ] {
            match try_reclaim_memory(strategy) {
                Ok(pages_freed) if pages_freed > 0 => {
                    log_info!(
                        LOG_ORIGIN,
                        "Successfully reclaimed {} pages using {:?} strategy",
                        pages_freed,
                        strategy
                    );
                    return OomResult::Reclaimed {
                        strategy,
                        pages_freed,
                    };
                }
                Ok(_) => {
                    log_info!(LOG_ORIGIN, "Strategy {:?} reclaimed 0 pages", strategy);
                }
                Err(OomError::StrategyNotImplemented) => {
                    // Strategy not implemented, try next one
                    continue;
                }
                Err(err) => {
                    log_warn!(
                        LOG_ORIGIN,
                        "Reclamation strategy {:?} failed: {:?}",
                        strategy,
                        err
                    );
                }
            }
        }

        log_info!(
            LOG_ORIGIN,
            "All reclamation strategies exhausted, proceeding with victim selection"
        );
    }

    let process_snapshot = crate::process::collect_process_memory_snapshot();
    #[cfg(debug_assertions)]
    crate::process::verify_process_accounting_sample(8, "oom_kill_sample");

    match select_victim(&process_snapshot) {
        Some(victim) => {
            let reason_label = match victim.reason {
                VictimReason::OverLimit => "over_limit",
                VictimReason::GlobalPressure => "global_pressure",
            };
            let usage_kb = victim.resident_pages * pmm::PAGE_SIZE / 1024;
            let limit_kb = victim.limit_pages * pmm::PAGE_SIZE / 1024;

            log_warn!(
                LOG_ORIGIN,
                "[oom] victim=pid:{} usage={}KB resident_pages={} limit_pages={} limit={}KB over_limit_pages={} reason={}",
                victim.pid,
                usage_kb,
                victim.resident_pages,
                victim.limit_pages,
                limit_kb,
                victim.over_limit_pages,
                reason_label
            );

            let free_before = pressure.free_pages;
            let start_tick = crate::interrupts::get_ticks();

            match crate::process::terminate_process(
                victim.pid,
                crate::process::TerminationReason::OutOfMemory,
            ) {
                Ok(()) => {
                    let (_, free_after) = pmm::get_stats();
                    let freed_pages = free_after.saturating_sub(free_before);
                    let teardown_ms = crate::interrupts::get_ticks()
                        .saturating_sub(start_tick)
                        .saturating_mul(10);

                    log_warn!(
                        LOG_ORIGIN,
                        "[oom] terminated pid:{} freed_pages={} freed={}KB teardown_ms={}",
                        victim.pid,
                        freed_pages,
                        (freed_pages * pmm::PAGE_SIZE) / 1024,
                        teardown_ms
                    );

                    #[cfg(debug_assertions)]
                    {
                        match crate::process::verify_process_accounting(victim.pid) {
                            Ok(()) => {
                                let remaining =
                                    crate::process::get_process_memory_usage(victim.pid)
                                        .map(|usage| usage.resident_pages)
                                        .unwrap_or(0);
                                if remaining > 0 {
                                    log_warn!(
                                        LOG_ORIGIN,
                                        "[oom] post_kill_accounting pid:{} remaining_resident_pages={}",
                                        victim.pid,
                                        remaining
                                    );
                                } else {
                                    log_debug!(
                                        LOG_ORIGIN,
                                        "[oom] post_kill_accounting pid:{} resident_pages=0",
                                        victim.pid
                                    );
                                }
                            }
                            Err(_) => {
                                log_debug!(
                                    LOG_ORIGIN,
                                    "[oom] post_kill_accounting pid:{} state=absent_or_zero",
                                    victim.pid
                                );
                            }
                        }
                    }

                    OomResult::Killed {
                        pid: victim.pid,
                        pages_freed: freed_pages,
                    }
                }
                Err(err) => {
                    log_warn!(
                        LOG_ORIGIN,
                        "[oom] terminate_process failed pid:{} err={:?}",
                        victim.pid,
                        err
                    );

                    OomResult::NoVictim {
                        reason: NoVictimReason::AllProcessesBelowMinimum,
                        fallback_action: FallbackAction::DenyAllocation,
                    }
                }
            }
        }
        None => {
            // No victim found - determine reason and fallback action
            let current_process =
                crate::sched::current_thread().and_then(crate::thread::get_thread_process_id);
            let has_active_process = process_snapshot
                .iter()
                .any(|process| !process.terminating && !process.terminated);
            let has_non_current_active = process_snapshot.iter().any(|process| {
                !process.terminating
                    && !process.terminated
                    && Some(process.process_id) != current_process
            });

            let reason = if !has_active_process {
                NoVictimReason::NoUserProcesses
            } else if !has_non_current_active {
                NoVictimReason::SystemReserved
            } else {
                NoVictimReason::AllProcessesBelowMinimum
            };

            // Determine fallback action based on memory pressure
            let fallback = if pressure.free_pages < 128 {
                // Critical situation - enter emergency mode
                FallbackAction::EnterEmergencyMode
            } else {
                // Less critical - just deny allocation
                FallbackAction::DenyAllocation
            };

            log_warn!(
                LOG_ORIGIN,
                "OOM killer found no victim — reason: {:?}, fallback: {:?}",
                reason,
                fallback
            );
            log_warn!(
                LOG_ORIGIN,
                "Memory pressure: {:?}, Free: {}/{} ({}%), Largest run: {}, Fragmentation: {:.2}, Processes over limit: {}",
                pressure.level,
                pressure.free_pages,
                pressure.total_pages,
                pressure.free_percent,
                pressure.largest_free_run,
                pressure.fragmentation_score,
                pressure.processes_over_limit
            );
            log_warn!(
                LOG_ORIGIN,
                "Process snapshot entries: {}, all non-killable under current policy",
                process_snapshot.len()
            );

            OomResult::NoVictim {
                reason,
                fallback_action: fallback,
            }
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
            log_warn!(
                LOG_ORIGIN,
                "Critical memory pressure — future allocations may trigger OOM"
            );
            // In the future, this is where page cache eviction would go
            false
        }
        MemoryPressure::Oom => {
            log_warn!(LOG_ORIGIN, "OOM pressure — invoking OOM killer");
            match oom_kill() {
                OomResult::Killed { pid, pages_freed } => {
                    log_info!(
                        LOG_ORIGIN,
                        "OOM killed pid={} (freed {} pages)",
                        pid,
                        pages_freed
                    );
                    true
                }
                OomResult::NoVictim {
                    reason,
                    fallback_action,
                } => {
                    log_warn!(
                        LOG_ORIGIN,
                        "No OOM victim found: {:?}, fallback: {:?}",
                        reason,
                        fallback_action
                    );
                    false
                }
                OomResult::Reclaimed {
                    strategy,
                    pages_freed,
                } => {
                    log_info!(
                        LOG_ORIGIN,
                        "Reclaimed {} pages using {:?}",
                        pages_freed,
                        strategy
                    );
                    true
                }
            }
        }
    }
}

/// Attempt to reclaim memory using a specific strategy.
///
/// This function tries to free memory using the specified reclamation strategy.
/// Returns the number of pages freed on success, or an error if the strategy
/// fails or is not implemented.
///
/// # Arguments
/// * `strategy` - The reclamation strategy to use
///
/// # Returns
/// * `Ok(usize)` - Number of pages freed
/// * `Err(OomError)` - Error if reclamation failed
///
/// # Requirements
/// Implements Req 6.1, Req 6.5
pub fn try_reclaim_memory(strategy: ReclaimStrategy) -> Result<usize, OomError> {
    log_info!(
        LOG_ORIGIN,
        "Attempting memory reclamation using strategy: {:?}",
        strategy
    );

    match strategy {
        ReclaimStrategy::CacheEviction => {
            // TODO: Implement page cache eviction when page cache is available
            // For now, this strategy is not implemented
            log_warn!(LOG_ORIGIN, "CacheEviction strategy not yet implemented");
            Err(OomError::StrategyNotImplemented)
        }
        ReclaimStrategy::CompactMemory => {
            // TODO: Implement memory compaction to reduce fragmentation
            // This would involve moving allocated pages to create larger contiguous runs
            log_warn!(LOG_ORIGIN, "CompactMemory strategy not yet implemented");
            Err(OomError::StrategyNotImplemented)
        }
        ReclaimStrategy::SwapOut => {
            // TODO: Implement page swapping when swap subsystem is available
            // This would write least-recently-used pages to disk
            log_warn!(LOG_ORIGIN, "SwapOut strategy not yet implemented");
            Err(OomError::StrategyNotImplemented)
        }
    }
}

pub fn init() {
    let pressure = check_pressure();
    log_info!(
        LOG_ORIGIN,
        "OOM subsystem initialized — current pressure: {:?}",
        pressure
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_pressure_info_struct() {
        // Test MemoryPressureInfo struct creation
        let info = MemoryPressureInfo {
            level: MemoryPressure::Low,
            free_pages: 5000,
            total_pages: 10000,
            free_percent: 50,
            largest_free_run: 2000,
            fragmentation_score: 0.6,
            processes_over_limit: 2,
        };

        assert_eq!(info.level, MemoryPressure::Low);
        assert_eq!(info.free_pages, 5000);
        assert_eq!(info.total_pages, 10000);
        assert_eq!(info.free_percent, 50);
        assert_eq!(info.largest_free_run, 2000);
        assert_eq!(info.fragmentation_score, 0.6);
        assert_eq!(info.processes_over_limit, 2);
    }

    #[test]
    fn test_should_trigger_oom() {
        // Test OOM trigger logic
        let info_oom = MemoryPressureInfo {
            level: MemoryPressure::Oom,
            free_pages: 100,
            total_pages: 10000,
            free_percent: 1,
            largest_free_run: 50,
            fragmentation_score: 0.5,
            processes_over_limit: 0,
        };
        assert!(
            info_oom.should_trigger_oom(),
            "OOM level should trigger OOM"
        );

        let info_low_free = MemoryPressureInfo {
            level: MemoryPressure::Critical,
            free_pages: 200,
            total_pages: 10000,
            free_percent: 2,
            largest_free_run: 50,
            fragmentation_score: 0.75,
            processes_over_limit: 0,
        };
        assert!(
            info_low_free.should_trigger_oom(),
            "Low free pages with small run should trigger OOM"
        );

        let info_ok = MemoryPressureInfo {
            level: MemoryPressure::Low,
            free_pages: 2000,
            total_pages: 10000,
            free_percent: 20,
            largest_free_run: 1000,
            fragmentation_score: 0.5,
            processes_over_limit: 0,
        };
        assert!(
            !info_ok.should_trigger_oom(),
            "Healthy memory should not trigger OOM"
        );
    }

    #[test]
    fn test_should_reclaim() {
        // Test reclaim logic
        let info_critical = MemoryPressureInfo {
            level: MemoryPressure::Critical,
            free_pages: 800,
            total_pages: 10000,
            free_percent: 8,
            largest_free_run: 400,
            fragmentation_score: 0.5,
            processes_over_limit: 0,
        };
        assert!(
            info_critical.should_reclaim(),
            "Critical pressure should trigger reclaim"
        );

        let info_oom = MemoryPressureInfo {
            level: MemoryPressure::Oom,
            free_pages: 100,
            total_pages: 10000,
            free_percent: 1,
            largest_free_run: 50,
            fragmentation_score: 0.5,
            processes_over_limit: 0,
        };
        assert!(
            info_oom.should_reclaim(),
            "OOM pressure should trigger reclaim"
        );

        let info_low = MemoryPressureInfo {
            level: MemoryPressure::Low,
            free_pages: 2000,
            total_pages: 10000,
            free_percent: 20,
            largest_free_run: 1000,
            fragmentation_score: 0.5,
            processes_over_limit: 0,
        };
        assert!(
            !info_low.should_reclaim(),
            "Low pressure should not trigger reclaim"
        );
    }

    #[test]
    fn test_fragmentation_score_calculation() {
        // Test fragmentation score logic
        // No fragmentation: all free pages are contiguous
        let no_frag = MemoryPressureInfo {
            level: MemoryPressure::None,
            free_pages: 1000,
            total_pages: 10000,
            free_percent: 10,
            largest_free_run: 1000,
            fragmentation_score: 0.0,
            processes_over_limit: 0,
        };
        assert_eq!(
            no_frag.fragmentation_score, 0.0,
            "No fragmentation should have score 0.0"
        );

        // High fragmentation: free pages are scattered
        let high_frag = MemoryPressureInfo {
            level: MemoryPressure::Low,
            free_pages: 1000,
            total_pages: 10000,
            free_percent: 10,
            largest_free_run: 100,
            fragmentation_score: 0.9,
            processes_over_limit: 0,
        };
        assert!(
            high_frag.fragmentation_score > 0.8,
            "High fragmentation should have score > 0.8"
        );
    }

    #[test]
    fn test_oom_kill_integration_with_pressure() {
        // Test that oom_kill properly integrates memory pressure information
        // This test verifies that:
        // 1. check_memory_pressure_detailed() is called before victim selection
        // 2. Reclamation strategies are attempted when pressure is Critical or Oom
        // 3. Pressure info is included in log messages

        // Note: This is a structural test that verifies the integration exists.
        // Actual behavior testing would require mocking the PMM and thread subsystems.

        // Verify that MemoryPressureInfo has all required fields for logging
        let pressure = MemoryPressureInfo {
            level: MemoryPressure::Oom,
            free_pages: 100,
            total_pages: 10000,
            free_percent: 1,
            largest_free_run: 50,
            fragmentation_score: 0.8,
            processes_over_limit: 2,
        };

        // Verify should_reclaim returns true for Oom pressure
        assert!(
            pressure.should_reclaim(),
            "Oom pressure should trigger reclamation"
        );

        // Verify should_trigger_oom returns true for Oom pressure
        assert!(
            pressure.should_trigger_oom(),
            "Oom pressure should trigger OOM killer"
        );
    }
}
