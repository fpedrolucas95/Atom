// Service Manager and Boot Manifest (Phase 6.3)
//
// Provides a declarative, capability-aware catalog of user-space services
// loaded by the init process. The service manager consumes a TOML-like
// manifest embedded in the kernel image, validates dependencies, resolves a
// deterministic startup order, and tracks lifecycle state transitions
// reported by services.
//
// Design goals:
// - Policy-free kernel core: the manifest declares which binaries run and the
//   minimal capabilities they receive.
// - Auditability: validation and startup planning are logged during boot.
// - Determinism: dependency resolution uses a stable topological order.
// - Safety: manifest parsing is strict and rejects malformed input early.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::{Mutex, Once};

use crate::{log_error, log_info, log_warn};

const LOG_ORIGIN: &str = "svcman";

const EMBEDDED_BOOT_MANIFEST: &str = r#"
[service.ui_shell]
binary = "/init/ui_shell.elf"
capabilities = ["FrameBufferCap", "PointerCap"]

[service.fs_server]
binary = "/init/fs.elf"
capabilities = ["MemRegionCap", "IPCPortCap"]
depends_on = ["storage_driver"]

[service.storage_driver]
binary = "/init/nvme_driver.elf"
capabilities = ["IRQCap:33", "DeviceCap:0000:01:00.0", "DMABufferCap"]
"#;

#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub name: String,
    pub binary: String,
    pub capabilities: Vec<String>,
    pub depends_on: Vec<String>,
}

impl ServiceSpec {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            binary: String::new(),
            capabilities: Vec::new(),
            depends_on: Vec::new(),
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum ManifestError {
    InvalidSection { line: usize, content: String },
    DuplicateService(String),
    OrphanKeyValue { line: usize, content: String },
    UnknownKey { key: String, line: usize },
    MissingBinary(String),
    UnknownDependency { service: String, depends_on: String },
    DependencyCycle(String),
    EmptyManifest,
}

pub struct BootManifest {
    services: BTreeMap<String, ServiceSpec>,
}

impl BootManifest {
    pub fn service(&self, name: &str) -> Option<&ServiceSpec> {
        self.services.get(name)
    }

    pub fn count(&self) -> usize {
        self.services.len()
    }

    pub fn startup_order(&self) -> Result<Vec<String>, ManifestError> {
        let mut order = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();

        for name in self.services.keys() {
            if !visited.contains(name) {
                self.visit(name, &mut visiting, &mut visited, &mut order)?;
            }
        }

        Ok(order)
    }

    fn visit(
        &self,
        name: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) -> Result<(), ManifestError> {
        if visiting.contains(name) {
            return Err(ManifestError::DependencyCycle(name.to_string()));
        }

        if visited.contains(name) {
            return Ok(());
        }

        visiting.insert(name.to_string());

        if let Some(spec) = self.services.get(name) {
            for dep in &spec.depends_on {
                if !self.services.contains_key(dep) {
                    return Err(ManifestError::UnknownDependency {
                        service: name.to_string(),
                        depends_on: dep.clone(),
                    });
                }
                self.visit(dep, visiting, visited, order)?;
            }
        }

        visiting.remove(name);
        visited.insert(name.to_string());
        order.push(name.to_string());

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ServiceState {
    Pending,
    Ready,
    Failed,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ServiceRuntime {
    pub state: ServiceState,
    pub granted_capabilities: Vec<String>,
}

pub struct ServiceManager {
    manifest: BootManifest,
    plan: Vec<String>,
    registry: Mutex<BTreeMap<String, ServiceRuntime>>,
}

impl ServiceManager {
    pub fn build(manifest: BootManifest) -> Result<Self, ManifestError> {
        if manifest.count() == 0 {
            return Err(ManifestError::EmptyManifest);
        }

        let plan = manifest.startup_order()?;
        let mut registry = BTreeMap::new();

        for (name, spec) in manifest.services.iter() {
            registry.insert(
                name.clone(),
                ServiceRuntime {
                    state: ServiceState::Pending,
                    granted_capabilities: spec.capabilities.clone(),
                },
            );
        }

        Ok(Self {
            manifest,
            plan,
            registry: Mutex::new(registry),
        })
    }

    pub fn manifest(&self) -> &BootManifest {
        &self.manifest
    }

    pub fn startup_plan(&self) -> &[String] {
        &self.plan
    }

    #[allow(dead_code)]
    pub fn service_state(&self, name: &str) -> Option<ServiceState> {
        let registry = self.registry.lock();
        registry.get(name).map(|rt| rt.state)
    }

    pub fn mark_ready(&self, name: &str) -> Result<(), LifecycleError> {
        let mut registry = self.registry.lock();
        let runtime = registry
            .get_mut(name)
            .ok_or(LifecycleError::UnknownService(name.to_string()))?;

        match runtime.state {
            ServiceState::Pending => {
                runtime.state = ServiceState::Ready;
                Ok(())
            }
            ServiceState::Ready => Ok(()),
            ServiceState::Failed => Err(LifecycleError::InvalidTransition {
                service: name.to_string(),
                from: ServiceState::Failed,
                to: ServiceState::Ready,
            }),
        }
    }

    #[allow(dead_code)]
    pub fn mark_failed(&self, name: &str) -> Result<(), LifecycleError> {
        let mut registry = self.registry.lock();
        let runtime = registry
            .get_mut(name)
            .ok_or(LifecycleError::UnknownService(name.to_string()))?;

        match runtime.state {
            ServiceState::Pending | ServiceState::Ready => {
                runtime.state = ServiceState::Failed;
                Ok(())
            }
            ServiceState::Failed => Ok(()),
        }
    }

    #[allow(dead_code)]
    pub fn planned_capabilities(&self, name: &str) -> Option<Vec<String>> {
        let registry = self.registry.lock();
        registry
            .get(name)
            .map(|rt| rt.granted_capabilities.clone())
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum LifecycleError {
    UnknownService(String),
    InvalidTransition {
        service: String,
        from: ServiceState,
        to: ServiceState,
    },
}

static SERVICE_MANAGER: Once<ServiceManager> = Once::new();

pub fn init_embedded_manifest() -> Result<&'static ServiceManager, ManifestError> {
    let manifest = parse_manifest(EMBEDDED_BOOT_MANIFEST)?;
    let manager = ServiceManager::build(manifest)?;
    Ok(SERVICE_MANAGER.call_once(|| manager))
}

pub fn manager() -> &'static ServiceManager {
    SERVICE_MANAGER
        .get()
        .expect("Service manager not initialized")
}

pub fn log_manifest_summary(manager: &ServiceManager) {
    log_info!(
        LOG_ORIGIN,
        "Service manifest loaded: {} services, {} planned start entries",
        manager.manifest().count(),
        manager.startup_plan().len()
    );

    for (idx, name) in manager.startup_plan().iter().enumerate() {
        if let Some(spec) = manager.manifest().service(name) {
            log_info!(
                LOG_ORIGIN,
                "{}. {} -> binary={}, caps={:?}, deps={:?}",
                idx + 1,
                spec.name,
                spec.binary,
                spec.capabilities,
                spec.depends_on
            );
        }
    }
}

pub fn parse_manifest(text: &str) -> Result<BootManifest, ManifestError> {
    let mut services: BTreeMap<String, ServiceSpec> = BTreeMap::new();
    let mut current_service: Option<String> = None;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') {
            if !line.ends_with(']') {
                return Err(ManifestError::InvalidSection {
                    line: line_no,
                    content: line.to_string(),
                });
            }

            let section = &line[1..line.len() - 1];
            if let Some(name) = section.strip_prefix("service.") {
                if services.contains_key(name) {
                    return Err(ManifestError::DuplicateService(name.to_string()));
                }
                services.insert(name.to_string(), ServiceSpec::new(name));
                current_service = Some(name.to_string());
            } else {
                return Err(ManifestError::InvalidSection {
                    line: line_no,
                    content: line.to_string(),
                });
            }
            continue;
        }

        let service_name = current_service.clone().ok_or_else(|| ManifestError::OrphanKeyValue {
            line: line_no,
            content: line.to_string(),
        })?;

        let (key, value) = parse_key_value(line, line_no)?;
        let spec = services
            .get_mut(&service_name)
            .expect("service should exist after section parse");

        match key {
            "binary" => {
                spec.binary = parse_string(value, line_no)?;
            }
            "capabilities" => {
                spec.capabilities = parse_array(value, line_no)?;
            }
            "depends_on" => {
                spec.depends_on = parse_array(value, line_no)?;
            }
            _ => {
                return Err(ManifestError::UnknownKey {
                    key: key.to_string(),
                    line: line_no,
                })
            }
        }
    }

    for spec in services.values() {
        if spec.binary.is_empty() {
            return Err(ManifestError::MissingBinary(spec.name.clone()));
        }
    }

    if services.is_empty() {
        return Err(ManifestError::EmptyManifest);
    }

    Ok(BootManifest { services })
}

fn parse_key_value(line: &str, line_no: usize) -> Result<(&str, &str), ManifestError> {
    if let Some((key, value)) = line.split_once('=') {
        Ok((key.trim(), value.trim()))
    } else {
        Err(ManifestError::OrphanKeyValue {
            line: line_no,
            content: line.to_string(),
        })
    }
}

fn parse_string(value: &str, line_no: usize) -> Result<String, ManifestError> {
    let trimmed = value.trim();
    if let Some(stripped) = trimmed.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Ok(stripped.to_string())
    } else {
        log_warn!(
            LOG_ORIGIN,
            "Line {}: expected quoted string, treating as raw",
            line_no
        );
        Ok(trimmed.to_string())
    }
}

fn parse_array(value: &str, line_no: usize) -> Result<Vec<String>, ManifestError> {
    let mut entries = Vec::new();
    let trimmed = value.trim();

    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(ManifestError::InvalidSection {
            line: line_no,
            content: trimmed.to_string(),
        });
    }

    let inner = &trimmed[1..trimmed.len() - 1];
    for item in inner.split(',') {
        let item_trimmed = item.trim();
        if item_trimmed.is_empty() {
            continue;
        }

        let parsed = if let Some(stripped) = item_trimmed
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
        {
            stripped.to_string()
        } else {
            item_trimmed.to_string()
        };

        entries.push(parsed);
    }

    Ok(entries)
}

pub fn initialize_and_report() {
    match init_embedded_manifest() {
        Ok(manager) => log_manifest_summary(manager),
        Err(err) => log_error!(LOG_ORIGIN, "Service manager initialization failed: {:?}", err),
    }
}
