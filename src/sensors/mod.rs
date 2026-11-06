//! Hardware metrics. RAM, disk and network are read through `sysinfo` on every
//! platform; everything else lives in a per-platform backend.

use anyhow::Result;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use sysinfo::{DiskUsage, Disks, MemoryRefreshKind, NetworkData, Networks, System};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

pub use platform::*;

/// Keys are updated one after another each tick, so readings closer together
/// than this belong to the same tick and are shared.
const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

static DISK_WRITE: SharedReading<u64> = SharedReading::new();
static DISK_READ: SharedReading<u64> = SharedReading::new();
static NET_RX: SharedReading<u64> = SharedReading::new();
static NET_TX: SharedReading<u64> = SharedReading::new();
static SYSINFO: Mutex<Option<System>> = Mutex::new(None);

/// The latest value of a metric calculated from the change in a counter.
///
/// The counters are process-wide, so if each key compared against the previous
/// read, keys showing the same metric would split the change between them.
/// Instead, one key's read per tick updates the value and the rest reuse it.
struct SharedReading<C> {
    latest: Mutex<Option<Reading<C>>>,
}

struct Reading<C> {
    taken: Instant,
    counters: C,
    value: f32,
}

impl<C> SharedReading<C> {
    const fn new() -> Self {
        Self {
            latest: Mutex::new(None),
        }
    }

    /// Return the current value, calling `read` for new counters and then
    /// `calculate(previous, current, elapsed_secs)` if the value is out of date.
    /// The first reading has nothing to compare against, so its value is 0.
    fn get(
        &self,
        read: impl FnOnce() -> C,
        calculate: impl FnOnce(&C, &C, f32) -> f32,
    ) -> f32 {
        let mut latest = self.latest.lock().unwrap();
        if let Some(reading) = latest.as_ref() {
            if reading.taken.elapsed() < MIN_SAMPLE_INTERVAL {
                return reading.value;
            }
        }

        let counters = read();
        let taken = Instant::now();
        let value = latest.as_ref().map_or(0.0, |previous| {
            let elapsed = taken.duration_since(previous.taken).as_secs_f32();
            calculate(&previous.counters, &counters, elapsed)
        });
        *latest = Some(Reading {
            taken,
            counters,
            value,
        });
        value
    }
}

impl SharedReading<u64> {
    /// Return the rate of change of a byte counter in MB/s.
    fn rate(&self, read: impl FnOnce() -> u64) -> f32 {
        self.get(read, |previous, current, elapsed| {
            current.saturating_sub(*previous) as f32 / 1_048_576.0 / elapsed
        })
    }
}

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
    Ok(DISK_WRITE.rate(|| disk_total(|usage| usage.total_written_bytes)))
}

/// Find disk read speed in MB/s
pub async fn find_disk_read() -> Result<f32> {
    Ok(DISK_READ.rate(|| disk_total(|usage| usage.total_read_bytes)))
}

fn disk_total(bytes: impl Fn(&DiskUsage) -> u64) -> u64 {
    let mut disks = Disks::new_with_refreshed_list();
    disks.refresh(true);
    disks
        .list()
        .iter()
        .filter(|disk| platform::counts_disk(disk.mount_point()))
        .map(|disk| bytes(&disk.usage()))
        .sum()
}

/// Find network download speed in MB/s
pub async fn find_net_download() -> Result<f32> {
    Ok(NET_RX.rate(|| network_total(NetworkData::total_received)))
}

/// Find network upload speed in MB/s
pub async fn find_net_upload() -> Result<f32> {
    Ok(NET_TX.rate(|| network_total(NetworkData::total_transmitted)))
}

fn network_total(bytes: impl Fn(&NetworkData) -> u64) -> u64 {
    let networks = Networks::new_with_refreshed_list();
    networks
        .iter()
        .filter(|(name, _)| platform::counts_interface(name))
        .map(|(_, network)| bytes(network))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::thread::sleep;

    #[test]
    fn keys_in_the_same_tick_share_one_reading() {
        let sampler = SharedReading::new();
        let reads = Cell::new(0u64);
        let read = || {
            reads.set(reads.get() + 1);
            reads.get() * 10_485_760 // 10 MiB more on each read
        };

        assert_eq!(sampler.rate(read), 0.0, "first reading has no baseline");
        assert_eq!(sampler.rate(read), 0.0, "still the same tick");
        assert_eq!(reads.get(), 1);

        sleep(MIN_SAMPLE_INTERVAL + Duration::from_millis(50));
        let first_key = sampler.rate(read);
        let second_key = sampler.rate(read);
        assert_eq!(reads.get(), 2);
        assert_eq!(first_key, second_key);
    }

    #[test]
    fn rate_is_measured_over_the_real_elapsed_time() {
        let sampler = SharedReading::new();
        sampler.rate(|| 0);
        sleep(MIN_SAMPLE_INTERVAL * 2);
        // 10 MiB over one second or so, not over an assumed one-second tick.
        let rate = sampler.rate(|| 10_485_760);
        assert!(rate > 5.0 && rate <= 10.0, "rate was {rate}");
    }
}
