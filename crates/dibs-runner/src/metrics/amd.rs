use super::{first_line, number};
use dibs_format::status::{GpuMetrics, Vendor};
use std::{fs, path::Path};

const AMD: &str = "0x1002";

/// Each AMD card amdgpu drives, from what it exports under `/sys/class/drm`.
pub fn cards() -> Vec<GpuMetrics> {
    fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_prefix("card")
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .filter_map(|e| card(&e.path().join("device")))
        .collect()
}

fn card(device: &Path) -> Option<GpuMetrics> {
    if first_line(device.join("vendor")).as_deref() != Some(AMD) {
        return None;
    }
    let pci = fs::read_link(device)
        .ok()?
        .file_name()?
        .to_string_lossy()
        .to_ascii_lowercase();
    let mut card = GpuMetrics::of(pci, Vendor::Amd);
    card.name = first_line(device.join("product_name")).filter(|n| !n.is_empty());
    // Reading a suspended card wakes it, which changes the machine being measured.
    if first_line(device.join("power/runtime_status")).as_deref() == Some("suspended") {
        card.suspended = true;
        return Some(card);
    }
    card.util_pct = number(device.join("gpu_busy_percent")).map(|p| p as u32);
    card.mem_used = number(device.join("mem_info_vram_used"));
    card.mem_total = number(device.join("mem_info_vram_total"));
    if let Some(hwmon) = fs::read_dir(device.join("hwmon"))
        .into_iter()
        .flatten()
        .flatten()
        .next()
        .map(|e| e.path())
    {
        card.temp_c = first_line(hwmon.join("temp1_input"))
            .and_then(|m| m.parse::<i64>().ok())
            .map(|milli| (milli / 1000) as i32);
        card.power_mw = number(hwmon.join("power1_average"))
            .or_else(|| number(hwmon.join("power1_input")))
            .map(|micro| micro / 1000);
        card.clock_mhz = number(hwmon.join("freq1_input")).map(|hz| (hz / 1_000_000) as u32);
    }
    Some(card)
}
