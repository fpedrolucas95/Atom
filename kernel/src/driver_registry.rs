//! Driver Registry
//!
//! This module maintains a registry of drivers loaded at boot time.
//! The bootloader loads ATXF files from the \drivers\ directory and
//! passes them to the kernel via BootInfo. This registry stores
//! references to those drivers so they can be spawned by userspace.

use crate::boot::{DriverList, DriverImage, ExecutableImage};
use spin::Once;

/// Global driver registry, initialized from boot info
static DRIVER_REGISTRY: Once<&'static DriverList> = Once::new();

/// Initialize the driver registry from boot info
pub fn init(drivers: &'static DriverList) {
    DRIVER_REGISTRY.call_once(|| drivers);

    crate::log_info!(
        "driver_registry",
        "Initialized with {} drivers",
        drivers.count
    );

    // Log each driver
    for driver in drivers.iter() {
        crate::log_debug!(
            "driver_registry",
            "  - {} ({} bytes)",
            driver.name_str(),
            driver.image.size
        );
    }
}

/// Find a driver by name
pub fn find_driver(name: &str) -> Option<&'static DriverImage> {
    DRIVER_REGISTRY.get().and_then(|registry| registry.find(name))
}

/// Get the executable image for a driver by name
pub fn get_driver_image(name: &str) -> Option<&'static ExecutableImage> {
    find_driver(name).map(|d| &d.image)
}

/// Get the number of registered drivers
pub fn driver_count() -> usize {
    DRIVER_REGISTRY.get().map(|r| r.count).unwrap_or(0)
}

/// List all driver names
pub fn list_drivers() -> impl Iterator<Item = &'static str> {
    DRIVER_REGISTRY.get()
        .map(|r| r.iter())
        .into_iter()
        .flatten()
        .map(|d| d.name_str())
}
