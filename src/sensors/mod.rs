//! Hardware metrics. RAM, disk and network are read through `sysinfo` on every
//! platform; everything else lives in a per-platform backend.

use anyhow::Result;
use std::sync::Mutex;
use sysinfo::{Disks, MemoryRefreshKind, Networks, System};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

pub use platform::*;

static PREV_DISK_WRITE: Mutex<Option<u64>> = Mutex::new(None);
static PREV_DISK_READ: Mutex<Option<u64>> = Mutex::new(None);
static PREV_NET_RX: Mutex<Option<u64>> = Mutex::new(None);
static PREV_NET_TX: Mutex<Option<u64>> = Mutex::new(None);
static SYSINFO: Mutex<Option<System>> = Mutex::new(None);

/// Represents a detected GPU with its card name and display name
#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub card: String,
    pub name: String,
}

/// Get RAM info (used, total) using a shared System instance.
fn ram_info() -> (u64, u64) {
    let mut guard = SYSINFO.lock().unwrap();
    let sys = guard.get_or_insert_with(System::new);
    sys.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    (sys.used_memory(), sys.total_memory())
}

/// Find RAM usage percentage
pub async fn find_ram_usage() -> Result<f32> {
    let (used, total) = ram_info();
    if total > 0 {
        Ok((used as f32 / total as f32) * 100.0)
    } else {
        Ok(0.0)
    }
}

/// Find RAM usage in gigabytes
pub async fn find_ram_usage_gb() -> Result<f32> {
    let (used, _) = ram_info();
    Ok(used as f32 / 1_073_741_824.0)
}

/// Find total RAM in gigabytes
pub async fn find_ram_total_gb() -> Result<f32> {
    let (_, total) = ram_info();
    Ok(total as f32 / 1_073_741_824.0)
}

/// Find disk write speed in MB/s
pub async fn find_disk_write() -> Result<f32> {
    let mut disks = Disks::new_with_refreshed_list();
    disks.refresh(true);

    let mut total_written = 0u64;
    for disk in disks.list() {
        if platform::counts_disk(disk.mount_point()) {
            total_written += disk.usage().total_written_bytes;
        }
    }

    let mut prev = PREV_DISK_WRITE.lock().unwrap();
    let write_speed = if let Some(prev_written) = *prev {
        let delta = total_written.saturating_sub(prev_written);
        (delta as f32) / 1_048_576.0 // Convert to MB/s
    } else {
        0.0
    };

    *prev = Some(total_written);
    Ok(write_speed)
}

/// Find disk read speed in MB/s
pub async fn find_disk_read() -> Result<f32> {
    let mut disks = Disks::new_with_refreshed_list();
    disks.refresh(true);

    let mut total_read = 0u64;
    for disk in disks.list() {
        if platform::counts_disk(disk.mount_point()) {
            total_read += disk.usage().total_read_bytes;
        }
    }

    let mut prev = PREV_DISK_READ.lock().unwrap();
    let read_speed = if let Some(prev_read) = *prev {
        let delta = total_read.saturating_sub(prev_read);
        (delta as f32) / 1_048_576.0 // Convert to MB/s
    } else {
        0.0
    };

    *prev = Some(total_read);
    Ok(read_speed)
}

/// Find network download speed in MB/s
pub async fn find_net_download() -> Result<f32> {
    let networks = Networks::new_with_refreshed_list();

    let mut total_rx = 0u64;
    for (interface_name, network) in &networks {
        if platform::counts_interface(interface_name) {
            total_rx += network.total_received();
        }
    }

    let mut prev = PREV_NET_RX.lock().unwrap();
    let download_speed = if let Some(prev_rx) = *prev {
        let delta = total_rx.saturating_sub(prev_rx);
        (delta as f32) / 1_048_576.0 // Convert to MB/s
    } else {
        0.0
    };

    *prev = Some(total_rx);
    Ok(download_speed)
}

/// Find network upload speed in MB/s
pub async fn find_net_upload() -> Result<f32> {
    let networks = Networks::new_with_refreshed_list();

    let mut total_tx = 0u64;
    for (interface_name, network) in &networks {
        if platform::counts_interface(interface_name) {
            total_tx += network.total_transmitted();
        }
    }

    let mut prev = PREV_NET_TX.lock().unwrap();
    let upload_speed = if let Some(prev_tx) = *prev {
        let delta = total_tx.saturating_sub(prev_tx);
        (delta as f32) / 1_048_576.0 // Convert to MB/s
    } else {
        0.0
    };

    *prev = Some(total_tx);
    Ok(upload_speed)
}
