use serde::{Deserialize, Serialize};
use sysinfo::Disks;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub file_system: String,
    pub is_removable: bool,
}

const SKIP_FS: &[&str] = &[
    "tmpfs", "devtmpfs", "proc", "sysfs", "cgroup", "cgroup2", "overlay", "squashfs", "devpts",
    "securityfs", "pstore", "bpf", "tracefs", "debugfs", "fusectl", "mqueue", "hugetlbfs",
];

pub fn list_disks() -> Vec<DiskInfo> {
    let disks = Disks::new_with_refreshed_list();
    let mut out = Vec::new();
    for d in disks.list() {
        let fs = d.file_system().to_string_lossy().to_string();
        if SKIP_FS.iter().any(|s| fs.eq_ignore_ascii_case(s)) {
            continue;
        }
        let mount = d.mount_point().to_string_lossy().to_string();
        if mount.starts_with("/snap") || mount.starts_with("/boot") || mount == "/dev" {
            continue;
        }
        let total = d.total_space();
        let available = d.available_space();
        if total == 0 {
            continue;
        }
        out.push(DiskInfo {
            name: d.name().to_string_lossy().to_string(),
            mount_point: mount,
            total_bytes: total,
            available_bytes: available,
            used_bytes: total.saturating_sub(available),
            file_system: fs,
            is_removable: d.is_removable(),
        });
    }
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out
}
