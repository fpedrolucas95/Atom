//! Filesystem integration for Desktop

#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use alloc::string::String;
use atom_syscall::fs::{readdir, open, close, OpenFlags};

pub struct AppInfo {
    pub name: String,
    pub path: String,
}

pub fn scan_apps() -> Vec<AppInfo> {
    let mut apps = Vec::new();
    if let Ok(fd) = open("/bin", OpenFlags::RDONLY, 0) {
        if let Ok(entries) = readdir(fd) {
            for entry in entries {
                apps.push(AppInfo {
                    name: entry.name.clone(),
                    path: String::from("/bin/") + &entry.name,
                });
            }
        }
        let _ = close(fd);
    }
    apps
}
