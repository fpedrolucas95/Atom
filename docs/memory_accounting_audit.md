# Memory Accounting Audit (Phase 1.5)

Status: `ENFORCED`
Date: `2026-04-04`
Owner: `kernel/src/process.rs`, `kernel/src/mm/vma.rs`, `kernel/src/shared_mem.rs`, `kernel/src/mm/oom.rs`

## Canonical Policy

- Source of truth is process-owned canonical accounting in `Process`:
  - `resident_private_pages`
  - `resident_shared_pages`
  - `reserved_bytes`
- OOM and policy consumers MUST use process snapshot APIs, never deep structural traversal.
- Shared memory policy: charge shared pages **per process mapping** (RSS semantics).
- COW policy:
  - fork converts private writable pages to COW-shared accounting for both processes;
  - write fault that breaks COW converts the writer page from shared to private.
- Reserved virtual memory policy:
  - tracks VMA-reserved bytes only;
  - shared-memory mapped pages are resident-only accounting, not `reserved_bytes`.

## Mutation Checklist

- [x] materialize (anon fault) -> accounting++ private
  - hook: `vma::materialize_anon` + `track_page`
- [x] unmap tracked page -> accounting-- (private/shared by source)
  - hook: `vma::take_materialized_page`
- [x] VMA destroy/drain tracked pages -> accounting-- for every drained page
  - hook: `vma::drain_materialized_pages`
- [x] VMA insert -> reserved_bytes++
  - hook: `vma::insert_vma`
- [x] VMA remove -> reserved_bytes--
  - hook: `vma::remove_vma`
- [x] VMA remove_range -> reserved_bytes-- aggregate
  - hook: `vma::remove_vma_range`
- [x] stack grow -> reserved_bytes++ by page
  - hook: `vma::grow_stack`
- [x] fork clone VMA set -> reserved_bytes inherited to child
  - hook: `vma::clone_vmas_for_fork`
- [x] fork COW/share materialization -> resident accounting applies to child and parent transitions
  - hook: `vma::upsert_materialized_page` (old->new reclassification)
- [x] COW break (write fault) -> shared->private transition on writer
  - hook: `vma::upsert_materialized_page` called from `materialize_cow`
- [x] eager executable/bootstrap mappings -> tracked explicitly
  - hook: `vma::account_pre_mapped_range`
  - call sites: `init_process` and `spawn_process_internal`
- [x] shared map -> accounting++ shared per mapped process
  - hook: `shared_mem::map_region`, `shared_mem::map_region_in_pml4`
- [x] shared unmap -> accounting-- shared per unmapped process
  - hook: `shared_mem::unmap_region`
- [x] shared cleanup on process teardown -> accounting-- for all active mappings
  - hook: `shared_mem::cleanup_process_shared_memory`
- [x] process teardown finalization -> accounting reset to zero
  - hook: `vma::destroy_process_vma_map`, `process::detach_thread_from_process`

## Drift Detection and Fail-Fast

- Slow-path verifier:
  - `process::verify_process_accounting(pid)`
  - recalculates real usage from:
    - page-table traversal: `thread::count_user_space_pages`
    - VMA/materialized pages: `vma::recalculate_process_vma_accounting`
    - shared mappings: `shared_mem::count_process_mapped_shared_pages`
- Fail-fast wrapper:
  - `process::verify_process_accounting_fail_fast(pid, context)`
  - logs structured drift line with canonical vs observed values and ownership anomalies
  - debug builds panic, release builds warn-only
- Current enforcement points:
  - post-process teardown in `thread::perform_final_cleanup`
  - debug sampling in OOM path via `process::verify_process_accounting_sample`

## Required Shared-Memory Scenarios (covered by hooks)

- [x] Two processes map same region -> each process shared accounting increments.
- [x] One process unmaps -> only that process shared accounting decrements.
- [x] Owner destroys region -> destroys when mappings gone; per-process decrements happen on unmap path.
- [x] Process dies with active mapping -> teardown cleanup unmaps and decrements all mapped shared pages.

## Invariant Alignment

- `INV-MEM-001`: process-centric accounting authority and update on transitions.
- `INV-LOCK-001`: snapshot + short lock scope; no deep lock recursion for OOM queries.
- `INV-OOM-001`: OOM decisions consume process snapshots, not structural traversal.
