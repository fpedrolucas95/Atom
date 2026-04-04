use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use super::{CapHandle, Capability, RevokeError};

#[derive(Debug, Clone)]
pub struct RevokePlan {
    pub root: CapHandle,
    pub nodes: Vec<CapHandle>,
    pub order: Vec<CapHandle>,
    pub missing: Vec<CapHandle>,
}

pub fn build_revoke_plan(root: CapHandle) -> Result<RevokePlan, RevokeError> {
    let snapshot = {
        let caps = super::CAPABILITY_MANAGER.global_caps.lock();
        caps.clone()
    };
    build_revoke_plan_from_snapshot(root, &snapshot)
}

pub(crate) fn build_revoke_plan_from_snapshot(
    root: CapHandle,
    snapshot: &BTreeMap<CapHandle, Capability>,
) -> Result<RevokePlan, RevokeError> {
    if !snapshot.contains_key(&root) {
        return Err(RevokeError::NotFound);
    }

    let mut nodes = Vec::new();
    let mut order = Vec::new();
    let mut visited = BTreeSet::new();
    let mut active = BTreeSet::new();
    let mut missing = BTreeSet::new();
    let mut stack: Vec<(CapHandle, bool)> = Vec::new();
    stack.push((root, false));

    while let Some((current, expanded)) = stack.pop() {
        if expanded {
            active.remove(&current);
            order.push(current);
            continue;
        }

        if visited.contains(&current) {
            continue;
        }

        let Some(cap) = snapshot.get(&current) else {
            missing.insert(current);
            continue;
        };

        if active.contains(&current) {
            return Err(RevokeError::DiscoveryInconsistent);
        }

        active.insert(current);
        visited.insert(current);
        nodes.push(current);
        stack.push((current, true));

        let mut children = cap.children.clone();
        children.sort_by_key(|handle| handle.raw());

        for child in children.into_iter().rev() {
            if active.contains(&child) {
                return Err(RevokeError::DiscoveryInconsistent);
            }
            if !snapshot.contains_key(&child) {
                missing.insert(child);
                continue;
            }
            if !visited.contains(&child) {
                stack.push((child, false));
            }
        }
    }

    Ok(RevokePlan {
        root,
        nodes,
        order,
        missing: missing.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cap::{CapPermissions, Capability, ProcessId, ResourceType, ThreadId};

    fn make_cap(
        handle: u64,
        owner: u64,
        parent: Option<u64>,
        children: &[u64],
    ) -> Capability {
        Capability {
            handle: CapHandle::from_raw(handle),
            resource: ResourceType::Thread(ThreadId::from_raw(handle)),
            permissions: CapPermissions::REVOKE,
            owner: ProcessId::from_raw(owner),
            parent: parent.map(CapHandle::from_raw),
            children: children.iter().copied().map(CapHandle::from_raw).collect(),
        }
    }

    #[test]
    fn simple_tree_produces_post_order_plan() {
        let mut snapshot = BTreeMap::new();
        snapshot.insert(CapHandle::from_raw(1), make_cap(1, 10, None, &[2, 3]));
        snapshot.insert(CapHandle::from_raw(2), make_cap(2, 20, Some(1), &[]));
        snapshot.insert(CapHandle::from_raw(3), make_cap(3, 30, Some(1), &[]));

        let plan = build_revoke_plan_from_snapshot(CapHandle::from_raw(1), &snapshot)
            .expect("plan should build");
        assert_eq!(
            plan.order,
            vec![
                CapHandle::from_raw(2),
                CapHandle::from_raw(3),
                CapHandle::from_raw(1),
            ]
        );
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn deep_tree_order_is_deterministic() {
        let mut snapshot = BTreeMap::new();
        snapshot.insert(CapHandle::from_raw(1), make_cap(1, 10, None, &[3, 2]));
        snapshot.insert(CapHandle::from_raw(2), make_cap(2, 10, Some(1), &[5, 4]));
        snapshot.insert(CapHandle::from_raw(3), make_cap(3, 10, Some(1), &[]));
        snapshot.insert(CapHandle::from_raw(4), make_cap(4, 10, Some(2), &[6]));
        snapshot.insert(CapHandle::from_raw(5), make_cap(5, 10, Some(2), &[]));
        snapshot.insert(CapHandle::from_raw(6), make_cap(6, 10, Some(4), &[]));

        let plan_a = build_revoke_plan_from_snapshot(CapHandle::from_raw(1), &snapshot)
            .expect("first plan should build");
        let plan_b = build_revoke_plan_from_snapshot(CapHandle::from_raw(1), &snapshot)
            .expect("second plan should build");
        assert_eq!(plan_a.order, plan_b.order);
    }

    #[test]
    fn missing_descendant_is_captured() {
        let mut snapshot = BTreeMap::new();
        snapshot.insert(CapHandle::from_raw(1), make_cap(1, 10, None, &[2]));

        let plan = build_revoke_plan_from_snapshot(CapHandle::from_raw(1), &snapshot)
            .expect("plan should build even with missing descendants");
        assert_eq!(plan.missing, vec![CapHandle::from_raw(2)]);
        assert_eq!(plan.order, vec![CapHandle::from_raw(1)]);
    }

    #[test]
    fn missing_root_returns_not_found() {
        let snapshot = BTreeMap::new();
        let err = build_revoke_plan_from_snapshot(CapHandle::from_raw(999), &snapshot)
            .expect_err("missing root must fail discovery");
        assert_eq!(err, RevokeError::NotFound);
    }

    #[test]
    fn cycle_is_rejected_during_discovery() {
        let mut snapshot = BTreeMap::new();
        snapshot.insert(CapHandle::from_raw(1), make_cap(1, 10, None, &[2]));
        snapshot.insert(CapHandle::from_raw(2), make_cap(2, 10, Some(1), &[1]));

        let err = build_revoke_plan_from_snapshot(CapHandle::from_raw(1), &snapshot)
            .expect_err("cycle must be rejected");
        assert_eq!(err, RevokeError::DiscoveryInconsistent);
    }
}
