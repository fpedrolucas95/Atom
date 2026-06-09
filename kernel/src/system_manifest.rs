// System Service Manifest
//
// Kernel-side root of trust for the userspace boot. This module is the single
// authoritative source for the identity and initial authority of every system
// service the kernel is willing to start.
//
// Why this exists
// ----------------
// Before this module, the first userspace process was treated as privileged
// purely by *order of creation* (`PRIVILEGED_PID`), and `SYS_SPAWN_*` were
// unrestricted. That delegated a critical authorization decision to userspace
// convention. The manifest moves that decision back into the kernel:
//
//   * `init` is registered by the kernel during boot with a fixed identity
//     (`SID_INIT`) loaded directly from the boot image — not by name, not by
//     PID, not by spawn order.
//   * Every other system service is declared here with a stable `ServiceId`,
//     a canonical name/image, its initial capabilities, and the set of
//     children it is allowed to start.
//   * The kernel grants a service's initial capabilities ONLY from this table,
//     and only when it itself spawns that service.
//
// Trust model (this PR)
// ---------------------
// ATXF signing and image hashing are out of scope for this PR, so `image_hash`
// is reserved (`None`) for now. `init`'s identity is strong regardless: it is
// the kernel-loaded boot image (`SpawnKind::BootImage`), never reachable by a
// userspace-named process. The remaining services derive their authority from
// the fact that only the kernel grants manifest capabilities and only to the
// process it is spawning — a process merely *named* `service_manager` cannot
// obtain `SpawnSystemService`, because it never passes through this grant path.
//
// A later PR will bind `image_hash`/signatures so the canonical image is
// cryptographically verified before grants are applied.

#![allow(dead_code)]

use crate::cap::{CapPermissions, ResourceType};
use crate::process::ProcessId;
use alloc::collections::BTreeMap;
use spin::Mutex;

const LOG_ORIGIN: &str = "manifest";

/// Stable, kernel-defined identity for a system service.
///
/// Identity is intentionally *not* derived from PID, process name, or boot
/// order. It is assigned by the kernel from this manifest when the service is
/// created and is the basis for `allowed_children` enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServiceId(pub u32);

/// How a service image is allowed to be brought into existence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnKind {
    /// Loaded directly by the kernel from the boot payload (only `init`).
    BootImage,
    /// Looked up by canonical name in the driver registry / `/drivers` and
    /// started via `SYS_SPAWN_PROCESS` by a holder of `SpawnSystemService`.
    SystemService,
}

/// A capability the kernel grants to a service at creation time.
///
/// Represented as a `(ResourceType, CapPermissions)` pair so it can live in a
/// `&'static` manifest table without allocating live `Capability` handles.
#[derive(Debug, Clone, Copy)]
pub struct InitialCapability {
    pub resource: ResourceType,
    pub permissions: CapPermissions,
}

impl InitialCapability {
    const fn new(resource: ResourceType, permissions: CapPermissions) -> Self {
        Self {
            resource,
            permissions,
        }
    }
}

/// A single entry in the kernel-side system service manifest.
pub struct SystemServiceManifestEntry {
    /// Stable kernel-defined identity.
    pub service_id: ServiceId,
    /// Canonical service name (used for `SYS_SPAWN_PROCESS` name lookup).
    pub canonical_name: &'static str,
    /// Canonical image location for this service.
    pub canonical_image: &'static str,
    /// Expected image hash. Reserved for a later PR (ATXF signing); `None` now.
    pub image_hash: Option<[u8; 32]>,
    /// How this service is allowed to be created.
    pub spawn_kind: SpawnKind,
    /// Additional names this service may register in `namesvc` besides its
    /// canonical name. Authoritative allowlist — a service may never register a
    /// name that is neither its canonical name nor one of these aliases.
    pub aliases: &'static [&'static str],
    /// Reserved IPC port ids (1..=255) this service is allowed to bind. Mirrors
    /// the `ReservedPort` capabilities granted in `initial_capabilities`.
    pub reserved_ports: &'static [u64],
    /// Capabilities granted by the kernel when this service is created.
    pub initial_capabilities: &'static [InitialCapability],
    /// Services this service is permitted to start (by identity).
    pub allowed_children: &'static [ServiceId],
}

// ── Stable service identities ──────────────────────────────────────────────

pub const SID_INIT: ServiceId = ServiceId(1);
pub const SID_SERVICE_MANAGER: ServiceId = ServiceId(2);
pub const SID_APP_LAUNCHER: ServiceId = ServiceId(3);
pub const SID_NAMESVC: ServiceId = ServiceId(4);
pub const SID_FSD: ServiceId = ServiceId(5);
pub const SID_NIC_DRIVER: ServiceId = ServiceId(6);
pub const SID_NETD: ServiceId = ServiceId(7);
pub const SID_UI_SHELL: ServiceId = ServiceId(8);
pub const SID_DISPLAY: ServiceId = ServiceId(9);

// Permission shorthands evaluated at compile time.
const SPAWN_PERM: CapPermissions = CapPermissions::EXECUTE;
const KLOG_PERM: CapPermissions = CapPermissions::READ;
const IDENTITY_PERM: CapPermissions = CapPermissions::READ;
const RESERVED_PORT_PERM: CapPermissions = CapPermissions::WRITE;

const fn identity(sid: ServiceId) -> InitialCapability {
    InitialCapability::new(ResourceType::ServiceIdentity { service_id: sid.0 }, IDENTITY_PERM)
}
const fn reserved(port_id: u64) -> InitialCapability {
    InitialCapability::new(ResourceType::ReservedPort { port_id }, RESERVED_PORT_PERM)
}

// Capability bundles. Every system service carries its own ServiceIdentity;
// spawn/klog/reserved-port authority is added only where the role requires it.
const INIT_CAPS: &[InitialCapability] = &[
    InitialCapability::new(ResourceType::SpawnSystemService, SPAWN_PERM),
    InitialCapability::new(ResourceType::ReadKernelLog, KLOG_PERM),
    identity(SID_INIT),
];
const SERVICE_MANAGER_CAPS: &[InitialCapability] = &[
    InitialCapability::new(ResourceType::SpawnSystemService, SPAWN_PERM),
    identity(SID_SERVICE_MANAGER),
    reserved(2),
];
const APP_LAUNCHER_CAPS: &[InitialCapability] = &[
    InitialCapability::new(ResourceType::SpawnFromPath, SPAWN_PERM),
    identity(SID_APP_LAUNCHER),
];
const NAMESVC_CAPS: &[InitialCapability] = &[identity(SID_NAMESVC), reserved(1)];
const FSD_CAPS: &[InitialCapability] = &[identity(SID_FSD), reserved(3)];
const NIC_DRIVER_CAPS: &[InitialCapability] = &[identity(SID_NIC_DRIVER)];
const NETD_CAPS: &[InitialCapability] = &[identity(SID_NETD)];
const UI_SHELL_CAPS: &[InitialCapability] = &[identity(SID_UI_SHELL)];
const DISPLAY_CAPS: &[InitialCapability] = &[identity(SID_DISPLAY)];

// Alias allowlists (names a service may register in namesvc beyond canonical).
const NO_ALIASES: &[&str] = &[];
// ui_shell is the compositor: it registers its compositor.* service names.
const UI_SHELL_ALIASES: &[&str] = &["compositor", "compositor.register", "compositor.wm"];

// Reserved port allowlists.
const NO_PORTS: &[u64] = &[];
const NAMESVC_PORTS: &[u64] = &[1];
const SERVICE_MANAGER_PORTS: &[u64] = &[2];
const FSD_PORTS: &[u64] = &[3];

// Children sets.
//
// `init` orchestrates the whole boot in the current design, so it may start
// every declared system service. `service_manager` may (re)start the same set
// of services *except* `init` and itself. Leaf services start nothing.
const INIT_CHILDREN: &[ServiceId] = &[
    SID_SERVICE_MANAGER,
    SID_APP_LAUNCHER,
    SID_NAMESVC,
    SID_FSD,
    SID_NIC_DRIVER,
    SID_NETD,
    SID_UI_SHELL,
    SID_DISPLAY,
];
const SERVICE_MANAGER_CHILDREN: &[ServiceId] = &[
    SID_APP_LAUNCHER,
    SID_NAMESVC,
    SID_FSD,
    SID_NIC_DRIVER,
    SID_NETD,
    SID_UI_SHELL,
    SID_DISPLAY,
];
const NO_CHILDREN: &[ServiceId] = &[];

/// The kernel-side system service manifest.
///
/// This table is the only source of initial authority for system services.
pub static SYSTEM_SERVICE_MANIFEST: &[SystemServiceManifestEntry] = &[
    SystemServiceManifestEntry {
        service_id: SID_INIT,
        canonical_name: "init",
        canonical_image: "boot:init.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::BootImage,
        aliases: NO_ALIASES,
        reserved_ports: NO_PORTS,
        initial_capabilities: INIT_CAPS,
        allowed_children: INIT_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_SERVICE_MANAGER,
        canonical_name: "service_manager",
        canonical_image: "/drivers/service_manager.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: NO_ALIASES,
        reserved_ports: SERVICE_MANAGER_PORTS,
        initial_capabilities: SERVICE_MANAGER_CAPS,
        allowed_children: SERVICE_MANAGER_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_APP_LAUNCHER,
        canonical_name: "app_launcher",
        canonical_image: "/drivers/app_launcher.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: NO_ALIASES,
        reserved_ports: NO_PORTS,
        initial_capabilities: APP_LAUNCHER_CAPS,
        allowed_children: NO_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_NAMESVC,
        canonical_name: "namesvc",
        canonical_image: "/drivers/namesvc.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: NO_ALIASES,
        reserved_ports: NAMESVC_PORTS,
        initial_capabilities: NAMESVC_CAPS,
        allowed_children: NO_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_FSD,
        canonical_name: "fsd",
        canonical_image: "/drivers/fsd.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: NO_ALIASES,
        reserved_ports: FSD_PORTS,
        initial_capabilities: FSD_CAPS,
        allowed_children: NO_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_NIC_DRIVER,
        canonical_name: "nic_driver",
        canonical_image: "/drivers/nic_driver.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: NO_ALIASES,
        reserved_ports: NO_PORTS,
        initial_capabilities: NIC_DRIVER_CAPS,
        allowed_children: NO_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_NETD,
        canonical_name: "netd",
        canonical_image: "/drivers/netd.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: NO_ALIASES,
        reserved_ports: NO_PORTS,
        initial_capabilities: NETD_CAPS,
        allowed_children: NO_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_UI_SHELL,
        canonical_name: "ui_shell",
        canonical_image: "/drivers/ui_shell.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: UI_SHELL_ALIASES,
        reserved_ports: NO_PORTS,
        initial_capabilities: UI_SHELL_CAPS,
        allowed_children: NO_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_DISPLAY,
        canonical_name: "display",
        canonical_image: "/drivers/display.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: NO_ALIASES,
        reserved_ports: NO_PORTS,
        initial_capabilities: DISPLAY_CAPS,
        allowed_children: NO_CHILDREN,
    },
];

/// Look up a manifest entry by its canonical service name.
pub fn lookup_by_name(name: &str) -> Option<&'static SystemServiceManifestEntry> {
    SYSTEM_SERVICE_MANIFEST
        .iter()
        .find(|entry| entry.canonical_name == name)
}

/// Look up a manifest entry by its stable identity.
pub fn lookup_by_id(id: ServiceId) -> Option<&'static SystemServiceManifestEntry> {
    SYSTEM_SERVICE_MANIFEST
        .iter()
        .find(|entry| entry.service_id == id)
}

/// Returns `true` if `name` is the canonical name or a declared alias for the
/// service identified by `service_id`. This is the authoritative allowlist the
/// kernel exposes to `namesvc`: a service may only register names that resolve
/// to its own kernel-assigned identity.
pub fn service_name_allowed(service_id: ServiceId, name: &str) -> bool {
    match lookup_by_id(service_id) {
        Some(entry) => entry.canonical_name == name || entry.aliases.contains(&name),
        None => false,
    }
}

/// Returns `true` if `service_id` is declared to bind reserved `port_id`.
pub fn service_owns_reserved_port(service_id: ServiceId, port_id: u64) -> bool {
    match lookup_by_id(service_id) {
        Some(entry) => entry.reserved_ports.contains(&port_id),
        None => false,
    }
}

// ── Runtime identity table ──────────────────────────────────────────────────
//
// Maps a live process to the system-service identity the kernel assigned when
// it created the process. This is the kernel's record of "who is who"; it is
// never set by userspace.

static SERVICE_IDENTITY: Mutex<BTreeMap<ProcessId, ServiceId>> = Mutex::new(BTreeMap::new());

/// Assign a kernel-defined service identity to `process`. Called only from the
/// kernel-controlled boot/spawn paths.
pub fn assign_service_identity(process: ProcessId, service_id: ServiceId) {
    let mut table = SERVICE_IDENTITY.lock();
    table.insert(process, service_id);
    drop(table);
    crate::log_info!(
        LOG_ORIGIN,
        "assigned service identity {:?} to process {}",
        service_id,
        process
    );
}

/// Return the service identity assigned to `process`, if any. A process with no
/// identity is an ordinary (non-service) process.
pub fn service_identity_of(process: ProcessId) -> Option<ServiceId> {
    SERVICE_IDENTITY.lock().get(&process).copied()
}

/// Drop a process's service identity on teardown so a future process can never
/// inherit authority via id/PID reuse.
pub fn clear_service_identity(process: ProcessId) {
    SERVICE_IDENTITY.lock().remove(&process);
}

/// Decision returned by [`authorize_system_service_spawn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnAuthError {
    /// The requested name is not a declared system service.
    NotDeclared,
    /// The caller has no kernel-assigned service identity.
    CallerNotService,
    /// The requested service is not an allowed child of the caller.
    NotAllowedChild,
}

/// Authorize a `SYS_SPAWN_PROCESS` request for system service `name` made by
/// `caller_process`.
///
/// This is enforcement *in addition to* the `SpawnSystemService` capability
/// gate in the syscall policy: the capability is necessary but not sufficient.
/// On success it returns the resolved manifest entry so the caller can apply
/// the declared initial capabilities to the new process.
pub fn authorize_system_service_spawn(
    caller_process: ProcessId,
    name: &str,
) -> Result<&'static SystemServiceManifestEntry, SpawnAuthError> {
    let entry = lookup_by_name(name).ok_or(SpawnAuthError::NotDeclared)?;

    let caller_id = service_identity_of(caller_process).ok_or(SpawnAuthError::CallerNotService)?;
    let caller_entry = lookup_by_id(caller_id).ok_or(SpawnAuthError::CallerNotService)?;

    if !caller_entry.allowed_children.contains(&entry.service_id) {
        return Err(SpawnAuthError::NotAllowedChild);
    }

    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_has_unique_identity() {
        for (i, a) in SYSTEM_SERVICE_MANIFEST.iter().enumerate() {
            for b in SYSTEM_SERVICE_MANIFEST.iter().skip(i + 1) {
                assert_ne!(a.service_id, b.service_id);
                assert_ne!(a.canonical_name, b.canonical_name);
            }
        }
    }

    #[test]
    fn init_is_boot_image_and_holds_only_bootstrap_authority() {
        let init = lookup_by_id(SID_INIT).expect("init must be declared");
        assert_eq!(init.spawn_kind, SpawnKind::BootImage);

        // init may spawn system services and read klog, but must NOT hold
        // SpawnFromPath — applications go through app_launcher.
        assert!(init
            .initial_capabilities
            .iter()
            .any(|c| c.resource == ResourceType::SpawnSystemService));
        assert!(init
            .initial_capabilities
            .iter()
            .any(|c| c.resource == ResourceType::ReadKernelLog));
        assert!(!init
            .initial_capabilities
            .iter()
            .any(|c| c.resource == ResourceType::SpawnFromPath));
    }

    #[test]
    fn app_launcher_can_spawn_paths_but_not_services() {
        let al = lookup_by_id(SID_APP_LAUNCHER).expect("app_launcher must be declared");
        assert!(al
            .initial_capabilities
            .iter()
            .any(|c| c.resource == ResourceType::SpawnFromPath));
        assert!(!al
            .initial_capabilities
            .iter()
            .any(|c| c.resource == ResourceType::SpawnSystemService));
    }

    #[test]
    fn service_manager_cannot_restart_init_or_itself() {
        let sm = lookup_by_id(SID_SERVICE_MANAGER).expect("service_manager must be declared");
        assert!(!sm.allowed_children.contains(&SID_INIT));
        assert!(!sm.allowed_children.contains(&SID_SERVICE_MANAGER));
    }

    #[test]
    fn leaf_services_carry_no_spawn_authority() {
        // Leaf services carry a ServiceIdentity (and maybe a ReservedPort), but
        // must never hold process-creation or klog authority, and must not be
        // allowed to start children.
        for id in [SID_NAMESVC, SID_FSD, SID_NIC_DRIVER, SID_NETD, SID_UI_SHELL, SID_DISPLAY] {
            let entry = lookup_by_id(id).expect("leaf service must be declared");
            assert!(!entry
                .initial_capabilities
                .iter()
                .any(|c| is_spawn_authority(&c.resource)));
            assert!(entry.allowed_children.is_empty());
        }
    }

    #[test]
    fn reserved_ports_match_capabilities() {
        // Every reserved port in `reserved_ports` must be backed by a matching
        // ReservedPort capability, and vice versa.
        for entry in SYSTEM_SERVICE_MANIFEST.iter() {
            for &p in entry.reserved_ports {
                assert!(
                    entry
                        .initial_capabilities
                        .iter()
                        .any(|c| matches!(c.resource, ResourceType::ReservedPort { port_id } if port_id == p)),
                    "service {:?} lists reserved port {} without a ReservedPort cap",
                    entry.service_id,
                    p
                );
            }
        }
        // Critical bootstrap ports are bound to the expected services only.
        assert!(service_owns_reserved_port(SID_NAMESVC, 1));
        assert!(service_owns_reserved_port(SID_SERVICE_MANAGER, 2));
        assert!(service_owns_reserved_port(SID_FSD, 3));
        assert!(!service_owns_reserved_port(SID_SERVICE_MANAGER, 1));
        assert!(!service_owns_reserved_port(SID_FSD, 1));
    }

    #[test]
    fn service_name_allowlist_enforced() {
        // Canonical names resolve to their own identity.
        assert!(service_name_allowed(SID_FSD, "fsd"));
        assert!(service_name_allowed(SID_APP_LAUNCHER, "app_launcher"));
        // ui_shell's compositor aliases are declared.
        assert!(service_name_allowed(SID_UI_SHELL, "compositor.wm"));
        assert!(service_name_allowed(SID_UI_SHELL, "compositor"));
        // Cross-identity registration is rejected: nic_driver cannot be "fsd".
        assert!(!service_name_allowed(SID_NIC_DRIVER, "fsd"));
        // Undeclared alias is rejected even for the right service.
        assert!(!service_name_allowed(SID_FSD, "filesystem"));
        assert!(!service_name_allowed(SID_DISPLAY, "compositor.wm"));
    }

    #[test]
    fn unknown_name_is_not_declared() {
        assert!(lookup_by_name("totally_not_a_service").is_none());
    }

    // ── Authorization tests (kernel-side spawn decision) ───────────────────

    /// Positive: init holds the SID_INIT identity and may start service_manager.
    #[test]
    fn init_may_start_service_manager() {
        let init_pid = ProcessId::from_raw(0xA001);
        assign_service_identity(init_pid, SID_INIT);

        let result = authorize_system_service_spawn(init_pid, "service_manager");
        assert!(matches!(result, Ok(e) if e.service_id == SID_SERVICE_MANAGER));

        clear_service_identity(init_pid);
    }

    /// Positive: service_manager may start a declared leaf service (fsd).
    #[test]
    fn service_manager_may_start_declared_service() {
        let sm_pid = ProcessId::from_raw(0xA002);
        assign_service_identity(sm_pid, SID_SERVICE_MANAGER);

        assert!(authorize_system_service_spawn(sm_pid, "fsd").is_ok());

        clear_service_identity(sm_pid);
    }

    /// Negative: service_manager may NOT restart init (not an allowed child).
    #[test]
    fn service_manager_cannot_start_init() {
        let sm_pid = ProcessId::from_raw(0xA003);
        assign_service_identity(sm_pid, SID_SERVICE_MANAGER);

        assert!(matches!(
            authorize_system_service_spawn(sm_pid, "init"),
            Err(SpawnAuthError::NotAllowedChild)
        ));

        clear_service_identity(sm_pid);
    }

    /// Negative: a process with no kernel-assigned identity cannot spawn any
    /// system service — even one that is perfectly well declared. This is the
    /// core "named like init but not actually init" protection.
    #[test]
    fn process_without_identity_cannot_spawn_service() {
        let rogue_pid = ProcessId::from_raw(0xA004);
        clear_service_identity(rogue_pid); // ensure no identity

        assert!(matches!(
            authorize_system_service_spawn(rogue_pid, "service_manager"),
            Err(SpawnAuthError::CallerNotService)
        ));
    }

    /// Negative: requesting an undeclared service name is rejected before any
    /// identity check.
    #[test]
    fn undeclared_service_is_rejected() {
        let init_pid = ProcessId::from_raw(0xA005);
        assign_service_identity(init_pid, SID_INIT);

        assert!(matches!(
            authorize_system_service_spawn(init_pid, "/apps/user/evil.atxf"),
            Err(SpawnAuthError::NotDeclared)
        ));

        clear_service_identity(init_pid);
    }

    /// Boot order is irrelevant: identity is assigned explicitly, so a process
    /// created earlier with no identity has no more authority than a later one.
    #[test]
    fn identity_not_derived_from_order() {
        let early_pid = ProcessId::from_raw(0xA006);
        let later_pid = ProcessId::from_raw(0xA007);

        // Only the later process is granted the service_manager identity.
        assign_service_identity(later_pid, SID_SERVICE_MANAGER);

        assert!(matches!(
            authorize_system_service_spawn(early_pid, "fsd"),
            Err(SpawnAuthError::CallerNotService)
        ));
        assert!(authorize_system_service_spawn(later_pid, "fsd").is_ok());

        clear_service_identity(later_pid);
    }
}
