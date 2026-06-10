use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::cap::{
    self, CallbackError, CallbackResult, CapHandle, CapPermissions, ResourceType, RevokeStatus,
};
use crate::log_info;
use crate::mm::{oom, pmm, vm, vma};
use crate::process::{self, ProcessId, ProcessTerminationError, TerminationReason};
use crate::sched;
use crate::thread::{self, CpuContext, Thread, ThreadId, ThreadPriority, ThreadState};

const LOG_ORIGIN: &str = "arch_invariants";

const CALLBACK_PORT_PROBE: u64 = 98_200;
const CALLBACK_PORT_REENTRANT: u64 = 98_201;

static ARCH_INVARIANTS_SERIAL: Mutex<()> = Mutex::new(());
static CALLBACK_FIXTURE: Mutex<Option<CallbackFixture>> = Mutex::new(None);
static CALLBACK_TRACE: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());
static CALLBACK_REENTRANT_REGISTERED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct CallbackFixture {
    pid: ProcessId,
    tid: ThreadId,
}

struct ProcessFixture {
    pid: ProcessId,
    pml4: usize,
    threads: Vec<ThreadId>,
}

fn align_page(addr: usize) -> usize {
    addr & !(pmm::PAGE_SIZE - 1)
}

fn callback_probe(handle: CapHandle) -> CallbackResult {
    CALLBACK_TRACE.lock().push(("probe", handle.raw()));

    if let Some(fixture) = *CALLBACK_FIXTURE.lock() {
        let _ = process::get_process(fixture.pid);
        let _ = thread::get_thread_state(fixture.tid);
        let _ = process::collect_process_memory_snapshot();
        let _ = vma::get_process_stats(fixture.pid);
    }

    Ok(())
}

fn callback_error(handle: CapHandle) -> CallbackResult {
    CALLBACK_TRACE.lock().push(("error", handle.raw()));
    Err(CallbackError::Recoverable(
        "architectural invariants callback failure",
    ))
}

fn callback_reentrant_register(handle: CapHandle) -> CallbackResult {
    CALLBACK_TRACE.lock().push(("reentrant", handle.raw()));

    if !CALLBACK_REENTRANT_REGISTERED.swap(true, Ordering::SeqCst) {
        cap::register_revocation_callback(
            ResourceType::IpcPort {
                port_id: CALLBACK_PORT_REENTRANT,
            },
            callback_late,
        );
    }

    Ok(())
}

fn callback_late(handle: CapHandle) -> CallbackResult {
    CALLBACK_TRACE.lock().push(("late", handle.raw()));
    Ok(())
}

fn alloc_test_pml4() -> usize {
    let pml4 = pmm::alloc_pages_zeroed(1).expect("arch_invariants: failed to allocate test pml4");
    vm::clone_kernel_mappings(pml4).expect("arch_invariants: failed to clone kernel mappings");
    pmm::register_active_pml4(pml4).expect("arch_invariants: failed to register active pml4");
    pml4
}

fn make_userspace_thread(tid: ThreadId, pid: ProcessId, pml4: usize) -> Thread {
    Thread {
        id: tid,
        process_id: Some(pid),
        state: ThreadState::Ready,
        context: alloc::boxed::Box::new(CpuContext::zero()),
        kernel_stack: 0,
        kernel_stack_size: 0,
        address_space: pml4 as u64,
        priority: ThreadPriority::Normal,
        name: "arch_inv",
        capability_table: cap::create_capability_table(tid),
        is_userspace: true,
        user_stack: None,
    }
}

fn setup_process_fixture(seed: u64, extra_threads: usize) -> ProcessFixture {
    let pid = ProcessId::from_raw(seed);
    let primary = ThreadId::from_raw(seed);
    let pml4 = alloc_test_pml4();
    let mut threads = Vec::with_capacity(extra_threads.saturating_add(1));

    thread::add_thread(make_userspace_thread(primary, pid, pml4));
    threads.push(primary);

    for idx in 0..extra_threads {
        let tid = ThreadId::from_raw(seed.saturating_add(idx as u64).saturating_add(1));
        thread::add_thread(make_userspace_thread(tid, pid, pml4));
        threads.push(tid);
    }

    vma::create_bootstrap_process_vma_map(pid, pml4);
    process::reset_process_memory_accounting(pid);
    let _ = process::set_process_memory_limit(pid, 0);

    ProcessFixture { pid, pml4, threads }
}

fn cleanup_process_fixture(fixture: &ProcessFixture) {
    // A fixture terminated through the production path has already transferred
    // address-space ownership to the reaper. Its physical PML4 frame may have
    // been reused by now, so a second unregister/free would corrupt the new
    // owner rather than merely being redundant.
    if process::get_process(fixture.pid).is_none() {
        return;
    }

    let _ = cap::revoke_all_process_capabilities(fixture.pid);

    let tracked_pages = vma::drain_materialized_pages(fixture.pml4);
    for account in tracked_pages {
        let _ = vm::unmap_page_in_pml4(fixture.pml4, account.vaddr.as_usize());
        let _ = vma::free_page_account(account);
    }

    let _ = vma::destroy_process_vma_map(fixture.pid);

    for tid in fixture.threads.iter().copied() {
        let _ = thread::remove_thread(tid);
    }

    process::PROCESS_REGISTRY.lock().remove(&fixture.pid);
    process::PML4_TO_PROCESS
        .lock()
        .remove(&(fixture.pml4 as u64));
    vma::destroy_vma_map(fixture.pml4);
    crate::shared_mem::forget_process_shared_memory_cleanup(fixture.pid);

    let _ = pmm::unregister_active_pml4(fixture.pml4);
    let _ = pmm::free_page(fixture.pml4);
}

/// Run the same two-phase finalization that production performs through the
/// idle/reaper path:
///
///   1. repeatedly give the reaper a chance to consume exited TCBs;
///   2. confirm every fixture thread has disappeared from THREAD_LIST;
///   3. detach each member from its Process membership list idempotently;
///   4. finalize the Process only after all members are gone.
///
/// SMP note:
/// `thread::reap_zombies()` may be serialized internally or another CPU may be
/// in the middle of consuming the zombie list. Therefore a single call is not a
/// stable synchronization point. The bounded retry loop below keeps the
/// invariant deterministic without requiring a scheduler-specific barrier.
fn reap_fixture_zombies_and_finalize(fixture: &ProcessFixture) {
    const MAX_REAP_ROUNDS: usize = 65536;

    for round in 0..MAX_REAP_ROUNDS {
        thread::reap_zombies();

        let all_threads_reaped = fixture
            .threads
            .iter()
            .copied()
            .all(|tid| thread::find_thread(tid).is_none());

        if all_threads_reaped {
            for tid in fixture.threads.iter().copied() {
                // Idempotent by design: production reaper may already have
                // detached this member. Calling it here makes the test model
                // robust across both serialized and opportunistic reapers.
                process::detach_thread_from_process(fixture.pid, tid);
            }

            let finalized = process::try_finalize_process_after_reap(fixture.pid);
            assert!(
                finalized || process::get_process(fixture.pid).is_none(),
                "arch_invariants: process {} should finalize after all zombie threads are reaped",
                fixture.pid
            );
            return;
        }

        // Yield occasionally to allow reaper to run on another CPU in SMP
        if round % 256 == 0 {
            for _ in 0..100 {
                core::hint::spin_loop();
            }
        } else {
            core::hint::spin_loop();
        }
    }

    for tid in fixture.threads.iter().copied() {
        assert!(
            thread::find_thread(tid).is_none(),
            "arch_invariants: thread {} should have been reaped after bounded SMP reap wait",
            tid
        );
    }

    panic!(
        "arch_invariants: process {} still had unreaped zombie threads after {} reap rounds",
        fixture.pid, MAX_REAP_ROUNDS
    );
}

fn install_revoke_tree(owner_tid: ThreadId, resource_port: u64) -> CapHandle {
    let root_perms = CapPermissions::READ
        .union(CapPermissions::REVOKE)
        .union(CapPermissions::GRANT);

    let root = cap::create_root_capability(
        ResourceType::IpcPort {
            port_id: resource_port,
        },
        owner_tid,
        root_perms,
    )
    .expect("arch_invariants: create_root_capability must succeed");

    let _ = thread::add_thread_capability(owner_tid, root.clone())
        .expect("arch_invariants: adding root capability to owner thread must succeed");

    let child_a = cap::derive_capability(
        root.handle,
        owner_tid,
        owner_tid,
        CapPermissions::READ.union(CapPermissions::GRANT),
    )
    .expect("arch_invariants: first child derivation must succeed");

    let _ = cap::derive_capability(root.handle, owner_tid, owner_tid, CapPermissions::READ)
        .expect("arch_invariants: sibling derivation must succeed");

    let _ = cap::derive_capability(child_a, owner_tid, owner_tid, CapPermissions::READ)
        .expect("arch_invariants: grandchild derivation must succeed");

    root.handle
}

fn test_interleaved_page_accounting_consistency() {
    let fixture = setup_process_fixture(980_010, 0);
    let base = align_page((atom_abi::USER_HEAP_START as usize).saturating_add(0x20_0000));
    const PAGES: usize = 48;

    vma::insert_process_vma(
        fixture.pid,
        vma::Vma {
            start: base,
            end: base + (PAGES * pmm::PAGE_SIZE),
            perms: vma::VmaPermissions::read_write(),
            backing: vma::VmaBacking::Anonymous,
            label: "arch_invariants_acc",
        },
    )
    .expect("arch_invariants: inserting accounting VMA must succeed");

    for i in 0..PAGES {
        let source = if i % 2 == 0 {
            vma::PageSource::Anonymous
        } else {
            vma::PageSource::Shared
        };

        let account = vma::PageAccount {
            owner_process: Some(fixture.pid),
            owner_address_space: fixture.pml4,
            vaddr: vma::VirtAddr::new(base + (i * pmm::PAGE_SIZE)),
            paddr: 0x3000_0000 + (i * pmm::PAGE_SIZE),
            source,
            share_state: if i % 2 == 0 {
                vma::PageShareState::Exclusive
            } else {
                vma::PageShareState::CowShared
            },
            vma_start: base,
        };

        vma::upsert_materialized_page(fixture.pml4, account)
            .expect("arch_invariants: materialized page upsert must succeed");

        if i % 8 == 0 {
            let _ = process::collect_process_memory_snapshot();
        }
    }

    let usage = process::get_process_memory_usage(fixture.pid)
        .expect("arch_invariants: process usage must exist");
    assert_eq!(usage.resident_pages, PAGES);
    assert_eq!(usage.resident_private_pages, PAGES / 2);
    assert_eq!(usage.resident_shared_pages, PAGES / 2);

    for i in (0..PAGES).rev() {
        let page = base + (i * pmm::PAGE_SIZE);
        assert!(
            vma::take_materialized_page(fixture.pml4, page).is_some(),
            "arch_invariants: tracked page must be removable during accounting teardown"
        );
    }

    let usage_after = process::get_process_memory_usage(fixture.pid)
        .expect("arch_invariants: process usage must still exist after page removal");
    assert_eq!(usage_after.resident_pages, 0);
    assert_eq!(usage_after.resident_private_pages, 0);
    assert_eq!(usage_after.resident_shared_pages, 0);

    cleanup_process_fixture(&fixture);
}

fn test_fault_admission_policy_materialization_split() {
    let fixture = setup_process_fixture(980_020, 0);
    let base = align_page((atom_abi::USER_HEAP_START as usize).saturating_add(0x24_0000));
    let second_page = base + pmm::PAGE_SIZE;

    vma::insert_process_vma(
        fixture.pid,
        vma::Vma {
            start: base,
            end: base + (2 * pmm::PAGE_SIZE),
            perms: vma::VmaPermissions::read_write(),
            backing: vma::VmaBacking::Anonymous,
            label: "arch_invariants_fault_rw",
        },
    )
    .expect("arch_invariants: inserting rw fault VMA must succeed");

    let resolved = vma::handle_page_fault(
        vma::FaultContext::from_x86_error(
            fixture.pid,
            vma::AddressSpaceId::new(fixture.pml4),
            base,
            0x6,
        ),
        0x6,
    );
    assert_eq!(resolved, vma::FaultResult::Resolved);
    assert!(vma::get_materialized_page(fixture.pml4, base).is_some());

    let usage_before_limit = process::get_process_memory_usage(fixture.pid)
        .expect("arch_invariants: process usage must exist before limit change");
    process::set_process_memory_limit(fixture.pid, usage_before_limit.resident_pages)
        .expect("arch_invariants: setting process memory limit must succeed");

    let denied = vma::handle_page_fault(
        vma::FaultContext::from_x86_error(
            fixture.pid,
            vma::AddressSpaceId::new(fixture.pml4),
            second_page,
            0x6,
        ),
        0x6,
    );
    assert_eq!(denied, vma::FaultResult::OutOfMemory);
    assert!(vma::get_materialized_page(fixture.pml4, second_page).is_none());

    let invalid_fault_addr = base + 0x100_000;
    let invalid = vma::handle_page_fault(
        vma::FaultContext::from_x86_error(
            fixture.pid,
            vma::AddressSpaceId::new(fixture.pml4),
            invalid_fault_addr,
            0x6,
        ),
        0x6,
    );
    assert_eq!(invalid, vma::FaultResult::InvalidAddress);

    let ro_base = base + 0x200_000;
    vma::insert_process_vma(
        fixture.pid,
        vma::Vma {
            start: ro_base,
            end: ro_base + pmm::PAGE_SIZE,
            perms: vma::VmaPermissions::READ,
            backing: vma::VmaBacking::Anonymous,
            label: "arch_invariants_fault_ro",
        },
    )
    .expect("arch_invariants: inserting ro fault VMA must succeed");

    let terminal = vma::handle_page_fault(
        vma::FaultContext::from_x86_error(
            fixture.pid,
            vma::AddressSpaceId::new(fixture.pml4),
            ro_base,
            0x6,
        ),
        0x6,
    );
    assert_eq!(terminal, vma::FaultResult::ProtectionViolation);

    cleanup_process_fixture(&fixture);
}

fn test_revoke_two_phase_callbacks_and_reports() {
    let fixture = setup_process_fixture(980_030, 0);
    let owner_tid = fixture.threads[0];

    *CALLBACK_FIXTURE.lock() = Some(CallbackFixture {
        pid: fixture.pid,
        tid: owner_tid,
    });
    CALLBACK_TRACE.lock().clear();
    CALLBACK_REENTRANT_REGISTERED.store(false, Ordering::SeqCst);

    cap::register_revocation_callback(
        ResourceType::IpcPort {
            port_id: CALLBACK_PORT_PROBE,
        },
        callback_probe,
    );
    cap::register_revocation_callback(
        ResourceType::IpcPort {
            port_id: CALLBACK_PORT_PROBE,
        },
        callback_error,
    );

    let root = install_revoke_tree(owner_tid, CALLBACK_PORT_PROBE);
    let plan =
        cap::revoke_plan::build_revoke_plan(root).expect("arch_invariants: revoke plan must build");
    let report = cap::revoke_capability(root, owner_tid)
        .expect("arch_invariants: revoke must return report");

    assert_eq!(report.status, RevokeStatus::Complete);
    assert_eq!(report.revoked, plan.order);
    assert!(report.missing.is_empty());
    assert!(report.failed.is_empty());
    assert_eq!(report.callbacks_executed(), report.revoked.len() * 2);
    assert_eq!(report.callback_failures(), report.revoked.len());

    let second = cap::revoke_capability(root, owner_tid)
        .expect("arch_invariants: second revoke must still return report");
    assert_eq!(second.status, RevokeStatus::Failed);
    assert!(second.revoked.is_empty());
    assert!(second.missing.contains(&root));

    cap::register_revocation_callback(
        ResourceType::IpcPort {
            port_id: CALLBACK_PORT_REENTRANT,
        },
        callback_reentrant_register,
    );

    let reentrant_root_a = install_revoke_tree(owner_tid, CALLBACK_PORT_REENTRANT);
    let first_reentrant = cap::revoke_capability(reentrant_root_a, owner_tid)
        .expect("arch_invariants: first reentrant revoke must succeed");
    assert_eq!(first_reentrant.status, RevokeStatus::Complete);
    assert_eq!(
        first_reentrant.callbacks_executed(),
        first_reentrant.revoked.len()
    );

    let reentrant_root_b = install_revoke_tree(owner_tid, CALLBACK_PORT_REENTRANT);
    let second_reentrant = cap::revoke_capability(reentrant_root_b, owner_tid)
        .expect("arch_invariants: second reentrant revoke must succeed");
    assert_eq!(second_reentrant.status, RevokeStatus::Complete);
    assert_eq!(
        second_reentrant.callbacks_executed(),
        second_reentrant.revoked.len() * 2
    );
    assert!(CALLBACK_TRACE
        .lock()
        .iter()
        .any(|(name, _)| *name == "late"));

    *CALLBACK_FIXTURE.lock() = None;
    cleanup_process_fixture(&fixture);
}

fn test_oom_semantics_single_and_multithread() {
    let heavy = setup_process_fixture(980_040, 0);
    let light = setup_process_fixture(980_041, 0);

    process::set_process_memory_limit(heavy.pid, 4)
        .expect("arch_invariants: heavy process limit must be set");
    process::account_process_resident_pages_add(heavy.pid, 8, 0);
    process::account_process_resident_pages_add(light.pid, 3, 0);

    match oom::oom_kill() {
        oom::OomResult::Killed { pid, .. } => {
            assert_eq!(pid, heavy.pid);
        }
        other => {
            panic!(
                "arch_invariants: expected OOM kill for heavy process, got {:?}",
                other
            );
        }
    }

    // With deferred zombie reaping, terminated threads stay in THREAD_LIST with
    // state=Exited until the idle/reaper path runs. Simulate that complete
    // two-phase teardown here.
    reap_fixture_zombies_and_finalize(&heavy);
    assert!(process::get_process_memory_usage(heavy.pid).is_none());
    let _ = process::collect_process_memory_snapshot();

    cleanup_process_fixture(&heavy);
    cleanup_process_fixture(&light);

    let multi = setup_process_fixture(980_050, 2);
    let survivor = setup_process_fixture(980_150, 0);

    process::account_process_resident_pages_add(multi.pid, 12, 0);
    process::account_process_resident_pages_add(survivor.pid, 1, 0);

    match oom::oom_kill() {
        oom::OomResult::Killed { pid, .. } => {
            assert_eq!(pid, multi.pid);
        }
        other => {
            panic!(
                "arch_invariants: expected OOM kill for multithread process, got {:?}",
                other
            );
        }
    }

    reap_fixture_zombies_and_finalize(&multi);
    for tid in multi.threads.iter().copied() {
        assert!(
            thread::find_thread(tid).is_none(),
            "arch_invariants: no multithread process thread may survive OOM teardown"
        );
    }
    assert!(process::get_process(multi.pid).is_none());

    let post_oom = setup_process_fixture(980_250, 0);
    assert!(
        process::get_process(post_oom.pid).is_some(),
        "arch_invariants: kernel must remain stable and accept new process after OOM"
    );

    cleanup_process_fixture(&multi);
    cleanup_process_fixture(&survivor);
    cleanup_process_fixture(&post_oom);
}

fn test_multithread_teardown_consistency_under_repeated_kill() {
    let fixture = setup_process_fixture(980_060, 3);

    process::account_process_resident_pages_add(fixture.pid, 6, 0);

    assert_eq!(
        process::terminate_process(fixture.pid, TerminationReason::OutOfMemory),
        Ok(())
    );

    let second = process::terminate_process(
        fixture.pid,
        TerminationReason::KilledByKernel { reason_code: 0xF8 },
    );
    assert!(
        matches!(
            second,
            Err(ProcessTerminationError::NotFound)
                | Err(ProcessTerminationError::AlreadyTerminated)
        ),
        "arch_invariants: repeated kill request must not resurrect process state"
    );

    reap_fixture_zombies_and_finalize(&fixture);
    for tid in fixture.threads.iter().copied() {
        assert!(
            thread::find_thread(tid).is_none(),
            "arch_invariants: thread {} should not survive teardown",
            tid
        );
    }
    assert!(process::get_process(fixture.pid).is_none());

    cleanup_process_fixture(&fixture);
}

fn test_smp_affinity_contracts() {
    let Some(current) = sched::current_thread() else {
        return;
    };

    let online = crate::smp::online_cpu_count();
    let all_mask = if online >= 64 {
        u64::MAX
    } else {
        (1u64 << online) - 1
    };

    assert!(
        sched::set_thread_affinity(current, all_mask.max(1)),
        "arch_invariants: set_thread_affinity must accept non-empty mask"
    );

    let readback = sched::get_thread_affinity(current);
    assert_eq!(
        readback,
        all_mask.max(1),
        "arch_invariants: affinity readback must match last write"
    );

    assert!(
        !sched::set_thread_affinity(current, 0),
        "arch_invariants: affinity mask 0 must be rejected"
    );
}

pub fn run_architectural_invariants_self_tests() {
    let _serial = ARCH_INVARIANTS_SERIAL.lock();

    log_info!(LOG_ORIGIN, "Running Phase 8 integration self-tests...");

    test_interleaved_page_accounting_consistency();
    test_fault_admission_policy_materialization_split();
    test_revoke_two_phase_callbacks_and_reports();
    test_oom_semantics_single_and_multithread();
    test_multithread_teardown_consistency_under_repeated_kill();
    test_smp_affinity_contracts();

    log_info!(
        LOG_ORIGIN,
        "Phase 8 integration self-tests passed (locking/oom/revoke/fault admission/smp-affinity)"
    );
}
