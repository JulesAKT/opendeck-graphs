//! macOS sensor backend, targeting Apple Silicon.
//!
//! Temperatures come from `sysinfo`, and GPU stats from the `IOAccelerator`
//! entry in the IORegistry. None of this needs root.

use super::{ram_info, GpuInfo, SYSINFO};
use crate::graph_data::MetricType;
use anyhow::Result;
use core_foundation::base::{kCFAllocatorDefault, CFType, CFTypeRef, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::{c_char, c_void};
use std::path::Path;
use std::sync::Mutex;
use sysinfo::{Components, System};

/// Metrics with no sensor on macOS, hidden from the property inspector.
pub const UNSUPPORTED_METRICS: &[MetricType] = &[
    MetricType::GpuTemp,
    MetricType::GpuPower,
    MetricType::MotherboardTemp,
    MetricType::SystemFan,
    MetricType::CpuVoltage,
    MetricType::RamTemp,
];

/// Apple Silicon has exactly one GPU, so every GPU metric ignores the card.
const GPU_CARD: &str = "gpu0";
const BYTES_PER_GB: f32 = 1_073_741_824.0;

static COMPONENTS: Mutex<Option<Components>> = Mutex::new(None);

#[allow(non_camel_case_types)]
type io_object_t = u32;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    /// Consumes `matching`, even on failure.
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: *mut c_void,
        existing: *mut io_object_t,
    ) -> i32;
    fn IOIteratorNext(iterator: io_object_t) -> io_object_t;
    fn IORegistryEntryCreateCFProperty(
        entry: io_object_t,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: io_object_t) -> i32;
}

/// `/` and `/System/Volumes/Data` are two views of the same APFS container and
/// report identical I/O counters, so only one of them may be counted.
pub(super) fn counts_disk(mount_point: &Path) -> bool {
    !mount_point.starts_with("/System/Volumes")
}

/// Physical interfaces are all `en*`. Everything else either duplicates traffic
/// already counted there (`utun*` VPNs, `bridge*`) or is local (`lo0`, `awdl*`).
pub(super) fn counts_interface(name: &str) -> bool {
    name.starts_with("en")
}

/// Run `read` on each `IOAccelerator` until one returns a value.
fn find_in_accelerators<T>(mut read: impl FnMut(io_object_t) -> Option<T>) -> Option<T> {
    // SAFETY: the name is NUL-terminated, the matching dictionary is consumed by
    // IOServiceGetMatchingServices, and every object returned is released once.
    unsafe {
        let matching = IOServiceMatching(c"IOAccelerator".as_ptr());
        if matching.is_null() {
            return None;
        }
        let mut iterator: io_object_t = 0;
        // 0 is kIOMainPortDefault.
        if IOServiceGetMatchingServices(0, matching, &mut iterator) != 0 {
            return None;
        }
        let mut found = None;
        loop {
            let entry = IOIteratorNext(iterator);
            if entry == 0 {
                break;
            }
            if found.is_none() {
                found = read(entry);
            }
            IOObjectRelease(entry);
        }
        IOObjectRelease(iterator);
        found
    }
}

fn registry_property(entry: io_object_t, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    // SAFETY: `entry` is live for this call and `key` outlives it. A non-null
    // result is a +1 reference, which CFType takes ownership of.
    unsafe {
        let value = IORegistryEntryCreateCFProperty(
            entry,
            key.as_concrete_TypeRef(),
            kCFAllocatorDefault as *const c_void,
            0,
        );
        (!value.is_null()).then(|| CFType::wrap_under_create_rule(value))
    }
}

/// Read one integer from the accelerator's `PerformanceStatistics` dictionary.
fn gpu_statistic(name: &str) -> Option<i64> {
    let key = CFString::new(name);
    find_in_accelerators(|entry| {
        let stats = registry_property(entry, "PerformanceStatistics")?;
        if !stats.instance_of::<CFDictionary>() {
            return None;
        }
        // SAFETY: just checked to be a dictionary. This retains its own
        // reference, and `stats` keeps the values alive while we read them.
        let stats = unsafe {
            CFDictionary::<CFString, CFType>::wrap_under_get_rule(stats.as_CFTypeRef() as _)
        };
        stats.find(&key)?.downcast::<CFNumber>()?.to_i64()
    })
}

fn gpu_model() -> Option<String> {
    find_in_accelerators(|entry| {
        registry_property(entry, "model")?
            .downcast::<CFString>()
            .map(|model| model.to_string())
    })
}

pub fn list_gpus() -> Vec<GpuInfo> {
    vec![GpuInfo {
        card: GPU_CARD.to_string(),
        name: gpu_model().unwrap_or_else(|| "GPU".to_string()),
    }]
}

pub async fn find_gpu_load(_card: &str) -> Result<f32> {
    match gpu_statistic("Device Utilization %") {
        Some(load) => Ok(load as f32),
        None => {
            log::warn!("GPU load sensor not found");
            Ok(0.0)
        }
    }
}

fn gpu_memory_bytes() -> Option<f32> {
    let bytes = gpu_statistic("In use system memory");
    if bytes.is_none() {
        log::warn!("GPU memory sensor not found");
    }
    bytes.map(|b| b as f32)
}

/// Apple Silicon has no dedicated VRAM, so GPU memory is shown against total RAM.
pub async fn find_gpu_vram(_card: &str) -> Result<f32> {
    let (_, total) = ram_info();
    match gpu_memory_bytes() {
        Some(used) if total > 0 => Ok(used / total as f32 * 100.0),
        _ => Ok(0.0),
    }
}

pub async fn find_gpu_vram_gb(_card: &str) -> Result<f32> {
    Ok(gpu_memory_bytes().unwrap_or(0.0) / BYTES_PER_GB)
}

pub async fn find_gpu_vram_total_gb(_card: &str) -> Result<f32> {
    super::find_ram_total_gb().await
}

pub async fn find_cpu_load() -> Result<f32> {
    let mut guard = SYSINFO.lock().unwrap();
    let sys = guard.get_or_insert_with(System::new);
    sys.refresh_cpu_usage();
    let load = sys.global_cpu_usage();
    Ok(if load.is_finite() { load } else { 0.0 })
}

/// Refresh the temperature sensors and return every (label, temperature) reading.
fn temperatures() -> Vec<(String, f32)> {
    let mut guard = COMPONENTS.lock().unwrap();
    let components = guard.get_or_insert_with(Components::new_with_refreshed_list);
    components.refresh(true);
    components
        .list()
        .iter()
        .filter_map(|c| Some((c.label().to_string(), c.temperature()?)))
        .collect()
}

/// Hottest of the SoC die sensors. Apple doesn't document which die area each
/// one covers, so this is the temperature of the chip rather than of the CPU
/// cores alone.
fn soc_temperature(readings: &[(String, f32)]) -> Option<f32> {
    readings
        .iter()
        .filter(|(label, _)| label.starts_with("PMU tdie"))
        .map(|(_, temp)| *temp)
        .reduce(f32::max)
}

fn nand_temperature(readings: &[(String, f32)]) -> Option<f32> {
    readings
        .iter()
        .find(|(label, _)| label.starts_with("NAND"))
        .map(|(_, temp)| *temp)
}

pub async fn find_cpu_temperature() -> Result<f32> {
    let temp = soc_temperature(&temperatures());
    if temp.is_none() {
        log::warn!("CPU temperature sensor not found");
    }
    Ok(temp.unwrap_or(0.0))
}

pub async fn find_nvme_temperature() -> Result<f32> {
    let temp = nand_temperature(&temperatures());
    if temp.is_none() {
        log::warn!("NVMe temperature sensor not found");
    }
    Ok(temp.unwrap_or(0.0))
}

// Everything below is listed in UNSUPPORTED_METRICS. These only run for a key
// whose settings came from another platform, and read as 0 like any missing sensor.

pub async fn find_gpu_temperature(_card: &str) -> Result<f32> {
    Ok(0.0)
}

pub async fn find_gpu_power(_card: &str) -> Result<f32> {
    Ok(0.0)
}

pub async fn find_motherboard_temperature() -> Result<f32> {
    Ok(0.0)
}

pub async fn find_system_fan_speed(_fan_number: u32) -> Result<f32> {
    Ok(0.0)
}

pub async fn find_cpu_voltage() -> Result<f32> {
    Ok(0.0)
}

pub async fn find_ram_temperature() -> Result<f32> {
    Ok(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(label: &str, temp: f32) -> (String, f32) {
        (label.to_string(), temp)
    }

    #[test]
    fn system_volumes_are_not_counted_twice() {
        assert!(counts_disk(Path::new("/")));
        assert!(counts_disk(Path::new("/Volumes/External")));
        assert!(!counts_disk(Path::new("/System/Volumes/Data")));
    }

    #[test]
    fn only_physical_interfaces_are_counted() {
        for name in ["en0", "en7"] {
            assert!(counts_interface(name), "{name}");
        }
        for name in ["lo0", "utun4", "awdl0", "llw0", "bridge0", "anpi0"] {
            assert!(!counts_interface(name), "{name}");
        }
    }

    #[test]
    fn soc_temperature_is_hottest_die_sensor() {
        let readings = [
            reading("PMU tdie1", 40.0),
            reading("PMU tdie2", 45.0),
            reading("PMU tdev1", 90.0),
            reading("gas gauge battery", 95.0),
        ];
        assert_eq!(soc_temperature(&readings), Some(45.0));
        assert_eq!(soc_temperature(&[reading("als-temp", 20.0)]), None);
    }

    #[test]
    fn nand_temperature_is_found_by_label() {
        let readings = [reading("PMU tdie1", 40.0), reading("NAND CH0 temp", 31.0)];
        assert_eq!(nand_temperature(&readings), Some(31.0));
        assert_eq!(nand_temperature(&[reading("PMU tdie1", 40.0)]), None);
    }

    /// Reads the real hardware: `cargo test -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs Apple Silicon hardware"]
    async fn live_sensors_report_plausible_values() {
        let gpus = list_gpus();
        println!("gpus: {gpus:?}");
        assert_ne!(gpus[0].name, "GPU");

        find_cpu_load().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let cpu = find_cpu_load().await.unwrap();
        let gpu = find_gpu_load(GPU_CARD).await.unwrap();
        let vram_pct = find_gpu_vram(GPU_CARD).await.unwrap();
        let vram_gb = find_gpu_vram_gb(GPU_CARD).await.unwrap();
        let vram_total = find_gpu_vram_total_gb(GPU_CARD).await.unwrap();
        let cpu_temp = find_cpu_temperature().await.unwrap();
        let nvme_temp = find_nvme_temperature().await.unwrap();
        println!(
            "cpu {cpu}% gpu {gpu}% vram {vram_pct}% {vram_gb}/{vram_total}GB \
             cpu temp {cpu_temp}C nvme temp {nvme_temp}C"
        );

        assert!(cpu > 0.0 && cpu <= 100.0);
        assert!((0.0..=100.0).contains(&gpu));
        assert!(vram_pct > 0.0 && vram_pct <= 100.0);
        assert!(vram_gb > 0.0 && vram_gb <= vram_total);
        assert!(cpu_temp > 10.0 && cpu_temp < 120.0);
        assert!(nvme_temp > 10.0 && nvme_temp < 120.0);
    }
}
