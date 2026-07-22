//! Memory, operating-system, and local-storage detection.

use sysinfo::{Disks, System};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    pub name: String,
    pub kind: String,
    pub file_system: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    pub os: String,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub disks: Vec<DiskInfo>,
}

pub fn detect_system() -> SystemInfo {
    let mut system = System::new();
    system.refresh_memory();
    let disks = Disks::new_with_refreshed_list()
        .list()
        .iter()
        .filter(|disk| disk.total_space() > 0)
        .map(|disk| DiskInfo {
            name: disk.name().to_string_lossy().into_owned(),
            kind: disk.kind().to_string(),
            file_system: disk.file_system().to_string_lossy().into_owned(),
            mount_point: disk.mount_point().to_string_lossy().into_owned(),
            total_bytes: disk.total_space(),
            available_bytes: disk.available_space(),
        })
        .collect();

    SystemInfo {
        os: System::long_os_version().unwrap_or_else(|| std::env::consts::OS.to_string()),
        total_memory_bytes: system.total_memory(),
        available_memory_bytes: system.available_memory(),
        disks,
    }
}
