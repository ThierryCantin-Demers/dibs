use serde::{Deserialize, Serialize};

/// What a machine's CPUs, memory and cards are doing, read when a caller asks for it: each
/// reading is a file or a library call in the runner, so it costs nothing beside a benchmark.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metrics {
    /// Thousandths of the CPU time spent busy since the reading a caller asking for metrics
    /// left before this one; absent for the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_busy_permille: Option<u32>,
    /// What that reading covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_over_ms: Option<u64>,
    /// Bytes, the page cache and anything else the kernel can reclaim not counted as used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<GpuMetrics>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<Disk>,
}

/// A filesystem a job writes to, each listed once, under the first role it plays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub role: DiskRole,
    pub mount: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fstype: Option<String>,
    /// Bytes.
    pub total: u64,
    /// Bytes this account may still write, before any quota.
    pub free: u64,
    /// Each quota that counts what this account writes there, with a limit: a full quota stops
    /// a job as a full disk does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotas: Vec<Quota>,
    /// Scratch's top-level entries that live here rather than on scratch's own filesystem, by
    /// a symlink or a mount: `ws` and `target` moved to a bigger array, for one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiskRole {
    /// `$DIBS_SCRATCH`: trees, build caches and logs.
    Scratch,
    /// The `TMPDIR` a job is given.
    Tmp,
    /// `/tmp`, which a tool that ignores `TMPDIR` writes to.
    SystemTmp,
    /// Where only some of scratch's entries live, which `paths` names.
    Linked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub kind: QuotaKind,
    /// The user, group or project id it is kept for.
    pub id: u32,
    /// Bytes: the hard limit, or the soft one where there is none.
    pub limit: u64,
    /// Bytes.
    pub used: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaKind {
    User,
    Group,
    Project,
}

/// One card, by its slot. A reading the driver does not give is absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuMetrics {
    /// `domain:bus:device.function`, lowercase, as the inventory records it.
    pub pci: String,
    pub vendor: Vendor,
    /// What `--device` names it by, where the caller's inventory has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Asleep, so nothing else was read: reading it would wake it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suspended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub util_pct: Option<u32>,
    /// Bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_mw: Option<u64>,
    /// The SM clock on NVIDIA, the graphics clock on AMD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_mhz: Option<u32>,
    /// What holds the clock down, empty when nothing does; absent where the driver does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Nvidia,
    Amd,
}

impl GpuMetrics {
    pub fn of(pci: String, vendor: Vendor) -> GpuMetrics {
        GpuMetrics {
            pci,
            vendor,
            alias: None,
            name: None,
            suspended: false,
            util_pct: None,
            mem_used: None,
            mem_total: None,
            temp_c: None,
            power_mw: None,
            clock_mhz: None,
            throttle: None,
        }
    }
}
