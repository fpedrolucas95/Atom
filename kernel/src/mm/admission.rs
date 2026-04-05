use crate::mm::oom::{self, MemoryPressure};
use crate::process::ProcessId;
use crate::{log_debug, log_info, log_warn};

const LOG_ORIGIN: &str = "mm_admission";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    Allow,
    DenyProcessLimit,
    DenyGlobalPressure,
    TriggerOOM,
    InvalidPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultType {
    NotPresentAnonymous,
    CowWrite,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultCharge {
    None,
    Pages(usize),
}

impl FaultCharge {
    #[inline]
    pub const fn resident_delta_pages(self) -> usize {
        match self {
            FaultCharge::None => 0,
            FaultCharge::Pages(pages) => pages,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryAdmissionContext {
    pub pid: ProcessId,
    pub charge: FaultCharge,
    pub fault_type: FaultType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessUsageSnapshot {
    resident_pages: usize,
    limit_pages: usize,
}

pub struct MemoryAdmission;

impl MemoryAdmission {
    pub fn evaluate(ctx: MemoryAdmissionContext) -> AdmissionDecision {
        let process_allows_memory_ops = crate::process::process_allows_memory_operations(ctx.pid);
        let usage =
            crate::process::get_process_memory_usage(ctx.pid).map(|usage| ProcessUsageSnapshot {
                resident_pages: usage.resident_pages,
                limit_pages: usage.limit_pages,
            });
        let pressure = oom::check_pressure();

        let decision = evaluate_with_observation(ctx, process_allows_memory_ops, usage, pressure);
        log_debug!(
            LOG_ORIGIN,
            "[admission] pid={} fault_type={:?} charge={:?} pressure={:?} decision={:?}",
            ctx.pid,
            ctx.fault_type,
            ctx.charge,
            pressure,
            decision
        );
        decision
    }

    pub fn trigger_oom(ctx: &MemoryAdmissionContext) {
        log_warn!(
            LOG_ORIGIN,
            "[admission] trigger_oom pid={} fault_type={:?} charge={:?}",
            ctx.pid,
            ctx.fault_type,
            ctx.charge
        );
        let _ = oom::oom_kill();
    }
}

fn evaluate_with_observation(
    ctx: MemoryAdmissionContext,
    process_allows_memory_ops: bool,
    usage: Option<ProcessUsageSnapshot>,
    pressure: MemoryPressure,
) -> AdmissionDecision {
    if !matches!(
        ctx.fault_type,
        FaultType::NotPresentAnonymous | FaultType::CowWrite
    ) {
        return AdmissionDecision::InvalidPolicy;
    }

    if !process_allows_memory_ops {
        return AdmissionDecision::InvalidPolicy;
    }

    let Some(usage) = usage else {
        return AdmissionDecision::InvalidPolicy;
    };

    let requested_pages = ctx.charge.resident_delta_pages();
    if requested_pages == 0 {
        // No resident growth planned: this fault should never be denied by
        // quota/pressure admission.
        return AdmissionDecision::Allow;
    }

    if usage.limit_pages != 0
        && usage.resident_pages.saturating_add(requested_pages) > usage.limit_pages
    {
        return AdmissionDecision::DenyProcessLimit;
    }

    match pressure {
        MemoryPressure::Oom => AdmissionDecision::TriggerOOM,
        MemoryPressure::Critical => AdmissionDecision::DenyGlobalPressure,
        MemoryPressure::None | MemoryPressure::Low => AdmissionDecision::Allow,
    }
}

pub fn init() {
    run_admission_self_tests();
    log_info!(
        LOG_ORIGIN,
        "Memory admission layer initialized — policy decisions centralized"
    );
}

fn run_admission_self_tests() {
    let pid = ProcessId::from_raw(0xAD01);
    let base_ctx = MemoryAdmissionContext {
        pid,
        charge: FaultCharge::Pages(1),
        fault_type: FaultType::NotPresentAnonymous,
    };

    // VMA válida + memória disponível -> Allow
    assert_eq!(
        evaluate_with_observation(
            base_ctx,
            true,
            Some(ProcessUsageSnapshot {
                resident_pages: 8,
                limit_pages: 0,
            }),
            MemoryPressure::Low
        ),
        AdmissionDecision::Allow
    );

    // VMA válida + limite excedido -> DenyProcessLimit
    assert_eq!(
        evaluate_with_observation(
            base_ctx,
            true,
            Some(ProcessUsageSnapshot {
                resident_pages: 32,
                limit_pages: 32,
            }),
            MemoryPressure::None
        ),
        AdmissionDecision::DenyProcessLimit
    );

    // Pressão global -> TriggerOOM ou DenyGlobalPressure
    assert_eq!(
        evaluate_with_observation(
            base_ctx,
            true,
            Some(ProcessUsageSnapshot {
                resident_pages: 4,
                limit_pages: 0,
            }),
            MemoryPressure::Oom
        ),
        AdmissionDecision::TriggerOOM
    );
    assert_eq!(
        evaluate_with_observation(
            base_ctx,
            true,
            Some(ProcessUsageSnapshot {
                resident_pages: 4,
                limit_pages: 0,
            }),
            MemoryPressure::Critical
        ),
        AdmissionDecision::DenyGlobalPressure
    );

    // Fault inválido -> InvalidPolicy
    assert_eq!(
        evaluate_with_observation(
            MemoryAdmissionContext {
                fault_type: FaultType::Invalid,
                ..base_ctx
            },
            true,
            Some(ProcessUsageSnapshot {
                resident_pages: 1,
                limit_pages: 0,
            }),
            MemoryPressure::None
        ),
        AdmissionDecision::InvalidPolicy
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> MemoryAdmissionContext {
        MemoryAdmissionContext {
            pid: ProcessId::from_raw(77),
            charge: FaultCharge::Pages(1),
            fault_type: FaultType::NotPresentAnonymous,
        }
    }

    #[test]
    fn valid_fault_with_headroom_is_allowed() {
        assert_eq!(
            evaluate_with_observation(
                test_ctx(),
                true,
                Some(ProcessUsageSnapshot {
                    resident_pages: 2,
                    limit_pages: 8,
                }),
                MemoryPressure::Low
            ),
            AdmissionDecision::Allow
        );
    }

    #[test]
    fn process_limit_exceeded_is_denied() {
        assert_eq!(
            evaluate_with_observation(
                test_ctx(),
                true,
                Some(ProcessUsageSnapshot {
                    resident_pages: 8,
                    limit_pages: 8,
                }),
                MemoryPressure::None
            ),
            AdmissionDecision::DenyProcessLimit
        );
    }

    #[test]
    fn global_pressure_escalates_to_oom_or_backpressure() {
        assert_eq!(
            evaluate_with_observation(
                test_ctx(),
                true,
                Some(ProcessUsageSnapshot {
                    resident_pages: 1,
                    limit_pages: 0,
                }),
                MemoryPressure::Critical
            ),
            AdmissionDecision::DenyGlobalPressure
        );
        assert_eq!(
            evaluate_with_observation(
                test_ctx(),
                true,
                Some(ProcessUsageSnapshot {
                    resident_pages: 1,
                    limit_pages: 0,
                }),
                MemoryPressure::Oom
            ),
            AdmissionDecision::TriggerOOM
        );
    }

    #[test]
    fn invalid_fault_returns_invalid_policy() {
        assert_eq!(
            evaluate_with_observation(
                MemoryAdmissionContext {
                    fault_type: FaultType::Invalid,
                    ..test_ctx()
                },
                true,
                Some(ProcessUsageSnapshot {
                    resident_pages: 1,
                    limit_pages: 0,
                }),
                MemoryPressure::None
            ),
            AdmissionDecision::InvalidPolicy
        );
    }

    #[test]
    fn zero_charge_cow_bypasses_quota_and_pressure_denials() {
        let zero_charge_ctx = MemoryAdmissionContext {
            pid: ProcessId::from_raw(77),
            charge: FaultCharge::None,
            fault_type: FaultType::CowWrite,
        };

        assert_eq!(
            evaluate_with_observation(
                zero_charge_ctx,
                true,
                Some(ProcessUsageSnapshot {
                    resident_pages: 8,
                    limit_pages: 8,
                }),
                MemoryPressure::Oom
            ),
            AdmissionDecision::Allow
        );
    }
}
