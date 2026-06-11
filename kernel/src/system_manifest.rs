// System Service Manifest
//
// Kernel-side root of trust for the userspace boot and the single authoritative
// source for the identity and *initial authority* of every process the kernel
// is willing to start.
//
// Design across PRs
// ------------------
//   * PR1 — identity + spawn authority: `init` is the kernel-loaded boot image
//     with a fixed `ServiceId`; system services are declared here and spawn is
//     gated by `SpawnSystemService` / `SpawnFromPath`.
//   * PR2 — authenticated IPC: each service declares `aliases` and
//     `reserved_ports`, and carries a `ServiceIdentity` + `ReservedPort` cap.
//   * PR3 — least privilege: ALL sensitive grants (framebuffer, input, video
//     mode, raw hardware, filesystem) are centralised here. There are no
//     ambient/default grants at spawn anymore. Each service receives exactly
//     the capabilities its profile declares; applications launched by path
//     receive nothing unless they match a declared `TrustedAppProfile`.
//
// Trust model (pre-signing): ATXF signing is out of scope, so `image_hash` is
// reserved and trusted-app identity is keyed by canonical path. This residual
// path-trust is explicit and confined to a small auditable table here; it will
// be replaced by image hashing/signing in a later PR.

#![allow(dead_code)]

use crate::cap::{CapPermissions, InputDeviceType, ResourceType};
use crate::process::ProcessId;
use alloc::collections::BTreeMap;
use spin::Mutex;

const LOG_ORIGIN: &str = "manifest";

/// Stable, kernel-defined identity for a system service. Not derived from PID,
/// name, or boot order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServiceId(pub u32);

/// How a service image is allowed to be brought into existence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnKind {
    /// Loaded directly by the kernel from the boot payload (only `init`).
    BootImage,
    /// Started via `SYS_SPAWN_PROCESS` by a holder of `SpawnSystemService`.
    SystemService,
}

/// A capability the kernel grants at creation time, as a static
/// `(ResourceType, CapPermissions)` pair.
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

/// An environment-derived grant: a capability whose concrete parameters
/// (framebuffer geometry, device BDFs) are only known at runtime and so cannot
/// be a static `ResourceType`. The kernel resolves these at spawn time. This is
/// still a manifest declaration — nothing is granted that a profile didn't ask
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvGrant {
    /// Map authority for the system framebuffer (geometry resolved from boot
    /// graphics info). Granted to the display owner / compositor only.
    FramebufferMap,
    /// `Device` capabilities for every PCI network-class (0x02) device. Granted
    /// to the NIC driver only — never a blanket grant for all BDFs.
    NetworkDevices,
}

/// A single entry in the kernel-side system service manifest.
pub struct SystemServiceManifestEntry {
    pub service_id: ServiceId,
    pub canonical_name: &'static str,
    pub canonical_image: &'static str,
    /// Expected image hash. Reserved for a later PR (ATXF signing); `None` now.
    pub image_hash: Option<[u8; 32]>,
    pub spawn_kind: SpawnKind,
    /// Names this service may register in `namesvc` besides its canonical name.
    pub aliases: &'static [&'static str],
    /// Reserved IPC port ids (1..=255) it may bind (mirrors ReservedPort caps).
    pub reserved_ports: &'static [u64],
    /// Static capabilities granted at creation.
    pub initial_capabilities: &'static [InitialCapability],
    /// Environment-derived grants resolved at creation.
    pub environment_grants: &'static [EnvGrant],
    /// Services this service is permitted to start (by identity).
    pub allowed_children: &'static [ServiceId],
}

/// The exact set of grants applied to a process at creation. Built from a
/// manifest entry (system service) or a trusted-app profile, and applied by the
/// single grant path in `spawn_process_internal` — there is no other source of
/// authority.
#[derive(Debug, Clone, Copy)]
pub struct SpawnGrants {
    pub service_id: Option<ServiceId>,
    pub capabilities: &'static [InitialCapability],
    pub environment_grants: &'static [EnvGrant],
}

impl SpawnGrants {
    /// Grants for an ordinary application launched by path: nothing sensitive.
    pub const NONE: SpawnGrants = SpawnGrants {
        service_id: None,
        capabilities: &[],
        environment_grants: &[],
    };
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
pub const SID_TIMESYNC: ServiceId = ServiceId(10);

// Permission shorthands.
const SPAWN_PERM: CapPermissions = CapPermissions::EXECUTE;
const KLOG_PERM: CapPermissions = CapPermissions::READ;
const IDENTITY_PERM: CapPermissions = CapPermissions::READ;
const RESERVED_PORT_PERM: CapPermissions = CapPermissions::WRITE;
const INPUT_PERM: CapPermissions = CapPermissions::READ;
const MODESET_PERM: CapPermissions = CapPermissions::EXECUTE;
const FS_PERM: CapPermissions = CapPermissions::READ
    .union(CapPermissions::WRITE)
    .union(CapPermissions::GRANT);

const fn identity(sid: ServiceId) -> InitialCapability {
    InitialCapability::new(
        ResourceType::ServiceIdentity { service_id: sid.0 },
        IDENTITY_PERM,
    )
}
const fn reserved(port_id: u64) -> InitialCapability {
    InitialCapability::new(ResourceType::ReservedPort { port_id }, RESERVED_PORT_PERM)
}
const fn keyboard() -> InitialCapability {
    InitialCapability::new(
        ResourceType::InputDevice {
            device_type: InputDeviceType::Keyboard,
        },
        INPUT_PERM,
    )
}
const fn mouse() -> InitialCapability {
    InitialCapability::new(
        ResourceType::InputDevice {
            device_type: InputDeviceType::Mouse,
        },
        INPUT_PERM,
    )
}
const MODESET: InitialCapability =
    InitialCapability::new(ResourceType::DisplayModeSet, MODESET_PERM);

// ── Per-service capability profiles (least privilege) ───────────────────────
//
// Every service carries its own ServiceIdentity. Spawn/klog authority, reserved
// ports, filesystem, framebuffer, input and mode-set are added ONLY where the
// role requires them. Nothing here is ambient.

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
const FSD_CAPS: &[InitialCapability] = &[
    identity(SID_FSD),
    reserved(3),
    InitialCapability::new(ResourceType::FsNamespace { namespace_id: 0 }, FS_PERM),
];
const NIC_DRIVER_CAPS: &[InitialCapability] = &[identity(SID_NIC_DRIVER)];
const NETD_CAPS: &[InitialCapability] = &[identity(SID_NETD)];
// ui_shell is the compositor / display server in the current design: it is the
// single owner of the framebuffer and the input router. Documented exception —
// these are declared here, never granted ambiently.
const UI_SHELL_CAPS: &[InitialCapability] = &[identity(SID_UI_SHELL), keyboard(), mouse(), MODESET];
const DISPLAY_CAPS: &[InitialCapability] = &[identity(SID_DISPLAY)];

// Environment-grant profiles.
const NO_ENV: &[EnvGrant] = &[];
const UI_SHELL_ENV: &[EnvGrant] = &[EnvGrant::FramebufferMap];
const DISPLAY_ENV: &[EnvGrant] = &[EnvGrant::FramebufferMap];
const NIC_DRIVER_ENV: &[EnvGrant] = &[EnvGrant::NetworkDevices];

// Alias allowlists.
const NO_ALIASES: &[&str] = &[];
const UI_SHELL_ALIASES: &[&str] = &["compositor", "compositor.register", "compositor.wm"];

// Reserved port allowlists.
const NO_PORTS: &[u64] = &[];
const NAMESVC_PORTS: &[u64] = &[1];
const SERVICE_MANAGER_PORTS: &[u64] = &[2];
const FSD_PORTS: &[u64] = &[3];

// Children sets.
const INIT_CHILDREN: &[ServiceId] = &[
    SID_SERVICE_MANAGER,
    SID_APP_LAUNCHER,
    SID_NAMESVC,
    SID_FSD,
    SID_NIC_DRIVER,
    SID_NETD,
    SID_TIMESYNC,
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
    SID_TIMESYNC,
];
const NO_CHILDREN: &[ServiceId] = &[];

/// The kernel-side system service manifest — the only source of initial
/// authority for system services.
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
        environment_grants: NO_ENV,
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
        environment_grants: NO_ENV,
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
        environment_grants: NO_ENV,
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
        environment_grants: NO_ENV,
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
        environment_grants: NO_ENV,
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
        environment_grants: NIC_DRIVER_ENV,
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
        environment_grants: NO_ENV,
        allowed_children: NO_CHILDREN,
    },
    SystemServiceManifestEntry {
        service_id: SID_TIMESYNC,
        canonical_name: "timesync",
        canonical_image: "/drivers/timesync.atxf",
        image_hash: None,
        spawn_kind: SpawnKind::SystemService,
        aliases: NO_ALIASES,
        reserved_ports: NO_PORTS,
        initial_capabilities: &[identity(SID_TIMESYNC)],
        environment_grants: NO_ENV,
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
        environment_grants: UI_SHELL_ENV,
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
        environment_grants: DISPLAY_ENV,
        allowed_children: NO_CHILDREN,
    },
];

// ── Trusted application profiles ────────────────────────────────────────────
//
// Applications are launched by path (`SYS_SPAWN_FROM_PATH`) and receive NO
// ambient authority. A very small allowlist of *trusted* application images may
// receive a specific, minimal capability set. Matching is by EXACT canonical
// path (never substring/name heuristics). This is the only sanctioned exception
// and is a declared, auditable residual path-trust (to be replaced by image
// signing later).

pub struct TrustedAppProfile {
    pub canonical_path: &'static str,
    pub capabilities: &'static [InitialCapability],
    pub environment_grants: &'static [EnvGrant],
}

// system_settings legitimately changes resolution → DisplayModeSet only.
// Video-mode queries are public, so it needs no further capability. It gets no
// framebuffer map and no input.
const SYSTEM_SETTINGS_CAPS: &[InitialCapability] = &[MODESET];

pub static TRUSTED_APPS: &[TrustedAppProfile] = &[TrustedAppProfile {
    canonical_path: "/apps/system/system_settings.atxf",
    capabilities: SYSTEM_SETTINGS_CAPS,
    environment_grants: NO_ENV,
}];

/// Look up a trusted-app profile by EXACT canonical path.
pub fn lookup_trusted_app(path: &str) -> Option<&'static TrustedAppProfile> {
    TRUSTED_APPS.iter().find(|p| p.canonical_path == path)
}

/// Grants for an application launched by `path`: a declared trusted profile, or
/// nothing.
pub fn app_grants_for_path(path: &str) -> SpawnGrants {
    match lookup_trusted_app(path) {
        Some(p) => SpawnGrants {
            service_id: None,
            capabilities: p.capabilities,
            environment_grants: p.environment_grants,
        },
        None => SpawnGrants::NONE,
    }
}

// ── Lookups ─────────────────────────────────────────────────────────────────

pub fn lookup_by_name(name: &str) -> Option<&'static SystemServiceManifestEntry> {
    SYSTEM_SERVICE_MANIFEST
        .iter()
        .find(|e| e.canonical_name == name)
}

pub fn lookup_by_id(id: ServiceId) -> Option<&'static SystemServiceManifestEntry> {
    SYSTEM_SERVICE_MANIFEST.iter().find(|e| e.service_id == id)
}

/// Grants declared for a system service entry.
pub fn grants_for_entry(entry: &'static SystemServiceManifestEntry) -> SpawnGrants {
    SpawnGrants {
        service_id: Some(entry.service_id),
        capabilities: entry.initial_capabilities,
        environment_grants: entry.environment_grants,
    }
}

/// Authoritative allowlist: is `name` the canonical name or a declared alias for
/// `service_id`? Used by namesvc to authorise registration.
pub fn service_name_allowed(service_id: ServiceId, name: &str) -> bool {
    match lookup_by_id(service_id) {
        Some(entry) => entry.canonical_name == name || entry.aliases.contains(&name),
        None => false,
    }
}

/// Is `service_id` declared to bind reserved `port_id`?
pub fn service_owns_reserved_port(service_id: ServiceId, port_id: u64) -> bool {
    match lookup_by_id(service_id) {
        Some(entry) => entry.reserved_ports.contains(&port_id),
        None => false,
    }
}

// ── Runtime identity table ──────────────────────────────────────────────────

static SERVICE_IDENTITY: Mutex<BTreeMap<ProcessId, ServiceId>> = Mutex::new(BTreeMap::new());

pub fn assign_service_identity(process: ProcessId, service_id: ServiceId) {
    SERVICE_IDENTITY.lock().insert(process, service_id);
    crate::log_info!(
        LOG_ORIGIN,
        "assigned service identity {:?} to process {}",
        service_id,
        process
    );
}

pub fn service_identity_of(process: ProcessId) -> Option<ServiceId> {
    SERVICE_IDENTITY.lock().get(&process).copied()
}

pub fn clear_service_identity(process: ProcessId) {
    SERVICE_IDENTITY.lock().remove(&process);
}

/// Decision returned by [`authorize_system_service_spawn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnAuthError {
    NotDeclared,
    CallerNotService,
    NotAllowedChild,
}

/// Authorize a `SYS_SPAWN_PROCESS` request for system service `name` by
/// `caller_process`. The `SpawnSystemService` capability gate is necessary but
/// not sufficient: the service must be declared and an allowed child of the
/// caller's kernel-assigned identity.
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
    fn identities_and_names_unique() {
        for (i, a) in SYSTEM_SERVICE_MANIFEST.iter().enumerate() {
            for b in SYSTEM_SERVICE_MANIFEST.iter().skip(i + 1) {
                assert_ne!(a.service_id, b.service_id);
                assert_ne!(a.canonical_name, b.canonical_name);
            }
        }
    }

    #[test]
    fn init_holds_only_bootstrap_authority() {
        let init = lookup_by_id(SID_INIT).unwrap();
        assert_eq!(init.spawn_kind, SpawnKind::BootImage);
        assert!(init
            .initial_capabilities
            .iter()
            .any(|c| c.resource == ResourceType::SpawnSystemService));
        assert!(!init.initial_capabilities.iter().any(|c| matches!(
            c.resource,
            ResourceType::Framebuffer { .. }
                | ResourceType::InputDevice { .. }
                | ResourceType::DisplayModeSet
        )));
        assert!(init.environment_grants.is_empty());
    }

    #[test]
    fn only_compositor_owns_framebuffer_and_input() {
        for entry in SYSTEM_SERVICE_MANIFEST.iter() {
            let has_fb = entry.environment_grants.contains(&EnvGrant::FramebufferMap);
            let has_input = entry
                .initial_capabilities
                .iter()
                .any(|c| matches!(c.resource, ResourceType::InputDevice { .. }));
            if entry.service_id == SID_UI_SHELL {
                assert!(has_fb, "compositor must map framebuffer");
                assert!(has_input, "compositor must route input");
            } else {
                assert!(!has_fb, "{:?} must not map framebuffer", entry.service_id);
                assert!(!has_input, "{:?} must not read input", entry.service_id);
            }
        }
    }

    #[test]
    fn nic_driver_gets_network_devices_only() {
        let nic = lookup_by_id(SID_NIC_DRIVER).unwrap();
        assert!(nic.environment_grants.contains(&EnvGrant::NetworkDevices));
        assert_eq!(nic.environment_grants.len(), 1);
    }

    #[test]
    fn fsd_has_fs_but_no_display_or_input() {
        let fsd = lookup_by_id(SID_FSD).unwrap();
        assert!(fsd
            .initial_capabilities
            .iter()
            .any(|c| matches!(c.resource, ResourceType::FsNamespace { .. })));
        assert!(fsd.environment_grants.is_empty());
        assert!(!fsd.initial_capabilities.iter().any(|c| matches!(
            c.resource,
            ResourceType::InputDevice { .. }
                | ResourceType::Framebuffer { .. }
                | ResourceType::DisplayModeSet
        )));
    }

    #[test]
    fn reserved_ports_match_caps_and_bindings() {
        for entry in SYSTEM_SERVICE_MANIFEST.iter() {
            for &p in entry.reserved_ports {
                assert!(entry.initial_capabilities.iter().any(
                    |c| matches!(c.resource, ResourceType::ReservedPort { port_id } if port_id == p)
                ));
            }
        }
        assert!(service_owns_reserved_port(SID_NAMESVC, 1));
        assert!(service_owns_reserved_port(SID_SERVICE_MANAGER, 2));
        assert!(service_owns_reserved_port(SID_FSD, 3));
        assert!(!service_owns_reserved_port(SID_FSD, 1));
    }

    #[test]
    fn name_allowlist_enforced() {
        assert!(service_name_allowed(SID_FSD, "fsd"));
        assert!(service_name_allowed(SID_UI_SHELL, "compositor.wm"));
        assert!(!service_name_allowed(SID_NIC_DRIVER, "fsd"));
        assert!(!service_name_allowed(SID_FSD, "filesystem"));
    }

    #[test]
    fn trusted_apps_are_exact_path_and_minimal() {
        let p = lookup_trusted_app("/apps/system/system_settings.atxf").unwrap();
        assert!(p
            .capabilities
            .iter()
            .all(|c| c.resource == ResourceType::DisplayModeSet));
        assert!(p.environment_grants.is_empty());
        assert!(lookup_trusted_app("/apps/user/terminal.atxf").is_none());
        let none = app_grants_for_path("/apps/user/browser.atxf");
        assert!(
            none.capabilities.is_empty()
                && none.environment_grants.is_empty()
                && none.service_id.is_none()
        );
    }

    #[test]
    fn service_manager_cannot_start_init() {
        let sm = ProcessId::from_raw(0xB001);
        assign_service_identity(sm, SID_SERVICE_MANAGER);
        assert!(matches!(
            authorize_system_service_spawn(sm, "init"),
            Err(SpawnAuthError::NotAllowedChild)
        ));
        clear_service_identity(sm);
    }

    #[test]
    fn process_without_identity_cannot_spawn_service() {
        let rogue = ProcessId::from_raw(0xB002);
        clear_service_identity(rogue);
        assert!(matches!(
            authorize_system_service_spawn(rogue, "service_manager"),
            Err(SpawnAuthError::CallerNotService)
        ));
    }
}
