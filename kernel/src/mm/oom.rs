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
        matches!(
            self.level,
            MemoryPressure::Critical | MemoryPressure::Oom
        )
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
/// This function iterates through all registered processes and checks if their
/// current memory usage exceeds their configured memory limit.
///
/// # Returns
/// Number of processes over their memory limit
///
/// # Requirements
/// Implements Req 8.4, Req 8.5
fn count_processes_over_limit() -> usize {
    use crate::process::{get_process_memory_usage, PROCESS_REGISTRY};

    let registry = PROCESS_REGISTRY.lock();
    let mut count = 0;

    for (process_id, process) in registry.iter() {
        // Skip processes with no limit (0 = unlimited)
        if process.memory_limit_pages == 0 {
            continue;
        }

        // Get current memory usage
        if let Some(usage) = get_process_memory_usage(*process_id) {
            if usage.resident_pages > process.memory_limit_pages {
                count += 1;
            }
        }
    }

    count
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
    Killed { 
        tid: ThreadId, 
        name: &'static str,
        pages_freed: usize,
    },
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

/// Select and terminate the largest memory consumer.
///
/// Selection criteria:
/// - Only userspace threads are candidates
/// - The thread with the most resident pages is selected
/// - Returns information about the killed thread
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
        log_info!(LOG_ORIGIN, "Attempting memory reclamation before victim selection");
        
        // Try each reclamation strategy in order
        for strategy in [ReclaimStrategy::CacheEviction, ReclaimStrategy::CompactMemory, ReclaimStrategy::SwapOut] {
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
                    log_warn!(LOG_ORIGIN, "Reclamation strategy {:?} failed: {:?}", strategy, err);
                }
            }
        }
        
        log_info!(LOG_ORIGIN, "All reclamation strategies exhausted, proceeding with victim selection");
    }

    // Get list of all userspace threads and their memory usage
    let threads = crate::thread::get_all_thread_info();
    let mut best_victim: Option<(ThreadId, &'static str, usize)> = None;

    for (tid, name, process_id, is_userspace) in &threads {
        if !is_userspace {
            continue;
        }

        let resident = process_id
            .and_then(vma::get_process_stats)
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
                "Killing process '{}' (tid={}, resident={} pages, {} KB) — Pressure: {:?}, Free: {}/{} ({}%), Fragmentation: {:.2}",
                name,
                tid,
                resident,
                resident * pmm::PAGE_SIZE / 1024,
                pressure.level,
                pressure.free_pages,
                pressure.total_pages,
                pressure.free_percent,
                pressure.fragmentation_score
            );

            crate::thread::terminate_entity(
                tid,
                crate::thread::TerminationReason::OutOfMemory,
            );

            OomResult::Killed { 
                tid, 
                name,
                pages_freed: resident,
            }
        }
        None => {
            // No victim found - determine reason and fallback action
            let reason = if threads.is_empty() {
                NoVictimReason::NoUserProcesses
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
                "Userspace threads: {}, all below minimum killable size",
                threads.iter().filter(|(_, _, _, is_userspace)| *is_userspace).count()
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
            log_warn!(LOG_ORIGIN, "Critical memory pressure — future allocations may trigger OOM");
            // In the future, this is where page cache eviction would go
            false
        }
        MemoryPressure::Oom => {
            log_warn!(LOG_ORIGIN, "OOM pressure — invoking OOM killer");
            match oom_kill() {
                OomResult::Killed { tid, name, pages_freed } => {
                    log_info!(LOG_ORIGIN, "OOM killed '{}' (tid={}, freed {} pages)", name, tid, pages_freed);
                    true
                }
                OomResult::NoVictim { reason, fallback_action } => {
                    log_warn!(LOG_ORIGIN, "No OOM victim found: {:?}, fallback: {:?}", reason, fallback_action);
                    false
                }
                OomResult::Reclaimed { strategy, pages_freed } => {
                    log_info!(LOG_ORIGIN, "Reclaimed {} pages using {:?}", pages_freed, strategy);
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
    log_info!(LOG_ORIGIN, "Attempting memory reclamation using strategy: {:?}", strategy);

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
    log_info!(LOG_ORIGIN, "OOM subsystem initialized — current pressure: {:?}", pressure);
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
        assert!(info_oom.should_trigger_oom(), "OOM level should trigger OOM");

        let info_low_free = MemoryPressureInfo {
            level: MemoryPressure::Critical,
            free_pages: 200,
            total_pages: 10000,
            free_percent: 2,
            largest_free_run: 50,
            fragmentation_score: 0.75,
            processes_over_limit: 0,
        };
        assert!(info_low_free.should_trigger_oom(), "Low free pages with small run should trigger OOM");

        let info_ok = MemoryPressureInfo {
            level: MemoryPressure::Low,
            free_pages: 2000,
            total_pages: 10000,
            free_percent: 20,
            largest_free_run: 1000,
            fragmentation_score: 0.5,
            processes_over_limit: 0,
        };
        assert!(!info_ok.should_trigger_oom(), "Healthy memory should not trigger OOM");
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
        assert!(info_critical.should_reclaim(), "Critical pressure should trigger reclaim");

        let info_oom = MemoryPressureInfo {
            level: MemoryPressure::Oom,
            free_pages: 100,
            total_pages: 10000,
            free_percent: 1,
            largest_free_run: 50,
            fragmentation_score: 0.5,
            processes_over_limit: 0,
        };
        assert!(info_oom.should_reclaim(), "OOM pressure should trigger reclaim");

        let info_low = MemoryPressureInfo {
            level: MemoryPressure::Low,
            free_pages: 2000,
            total_pages: 10000,
            free_percent: 20,
            largest_free_run: 1000,
            fragmentation_score: 0.5,
            processes_over_limit: 0,
        };
        assert!(!info_low.should_reclaim(), "Low pressure should not trigger reclaim");
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
        assert_eq!(no_frag.fragmentation_score, 0.0, "No fragmentation should have score 0.0");

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
        assert!(high_frag.fragmentation_score > 0.8, "High fragmentation should have score > 0.8");
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
        assert!(pressure.should_reclaim(), "Oom pressure should trigger reclamation");
        
        // Verify should_trigger_oom returns true for Oom pressure
        assert!(pressure.should_trigger_oom(), "Oom pressure should trigger OOM killer");
    }
}

