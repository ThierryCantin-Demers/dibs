use dibs_format::status::{GpuMetrics, Vendor};
use std::{
    ffi::{CStr, c_char, c_int, c_uint, c_ulonglong, c_void},
    mem::{size_of, transmute_copy},
};

/// The NVIDIA driver's own library, opened at run time: a machine with no NVIDIA card has none,
/// and the runner must still start there.
const LIBRARY: &CStr = c"libnvidia-ml.so.1";

type Device = *mut c_void;
type Status = c_int;
const SUCCESS: Status = 0;
const TEMPERATURE_GPU: c_int = 0;
const CLOCK_SM: c_int = 1;
/// `nvmlClocksEventReasons`, by bit.
const REASONS: [(u64, &str); 9] = [
    (0x1, "gpu_idle"),
    (0x2, "applications_clocks"),
    (0x4, "sw_power_cap"),
    (0x8, "hw_slowdown"),
    (0x10, "sync_boost"),
    (0x20, "sw_thermal"),
    (0x40, "hw_thermal"),
    (0x80, "hw_power_brake"),
    (0x100, "display_clocks"),
];

#[repr(C)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
struct Memory {
    total: c_ulonglong,
    free: c_ulonglong,
    used: c_ulonglong,
}

#[repr(C)]
struct PciInfo {
    bus_id_legacy: [c_char; 16],
    domain: c_uint,
    bus: c_uint,
    device: c_uint,
    pci_device_id: c_uint,
    pci_subsystem_id: c_uint,
    bus_id: [c_char; 32],
}

/// The library, initialised, and shut down and closed when dropped.
struct Nvml {
    library: *mut c_void,
}

impl Nvml {
    fn open() -> Option<Nvml> {
        // SAFETY: a NUL-terminated name; a null handle is checked before use.
        let library = unsafe { libc::dlopen(LIBRARY.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if library.is_null() {
            return None;
        }
        let nvml = Nvml { library };
        // SAFETY: nvmlInit_v2 takes nothing and returns a status.
        let init: unsafe extern "C" fn() -> Status = unsafe { nvml.symbol(c"nvmlInit_v2") }?;
        match unsafe { init() } {
            SUCCESS => Some(nvml),
            _ => {
                // SAFETY: opened above, and not initialised, so Drop must not shut it down.
                unsafe { libc::dlclose(nvml.library) };
                std::mem::forget(nvml);
                None
            }
        }
    }

    /// # Safety
    /// `F` must be the function pointer type the library declares under `name`.
    unsafe fn symbol<F: Copy>(&self, name: &CStr) -> Option<F> {
        // SAFETY: an open handle and a NUL-terminated name.
        let found = unsafe { libc::dlsym(self.library, name.as_ptr()) };
        (!found.is_null() && size_of::<F>() == size_of::<*mut c_void>())
            // SAFETY: the caller names the symbol's type, and the sizes match.
            .then(|| unsafe { transmute_copy::<*mut c_void, F>(&found) })
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: the library was initialised in `open`, so it is shut down once, then closed.
        unsafe {
            if let Some(shutdown) = self.symbol::<unsafe extern "C" fn() -> Status>(c"nvmlShutdown")
            {
                shutdown();
            }
            libc::dlclose(self.library);
        }
    }
}

/// Each NVIDIA card the driver sees, empty where there is no driver.
pub fn cards() -> Vec<GpuMetrics> {
    let Some(nvml) = Nvml::open() else {
        return Vec::new();
    };
    // SAFETY: each type is the one nvml.h declares for its name.
    unsafe { read(&nvml) }.unwrap_or_default()
}

/// # Safety
/// `nvml` is open and initialised.
unsafe fn read(nvml: &Nvml) -> Option<Vec<GpuMetrics>> {
    type Count = unsafe extern "C" fn(*mut c_uint) -> Status;
    type Handle = unsafe extern "C" fn(c_uint, *mut Device) -> Status;
    type Pci = unsafe extern "C" fn(Device, *mut PciInfo) -> Status;
    type Name = unsafe extern "C" fn(Device, *mut c_char, c_uint) -> Status;
    type Use = unsafe extern "C" fn(Device, *mut Utilization) -> Status;
    type Mem = unsafe extern "C" fn(Device, *mut Memory) -> Status;
    type Temp = unsafe extern "C" fn(Device, c_int, *mut c_uint) -> Status;
    type Power = unsafe extern "C" fn(Device, *mut c_uint) -> Status;
    type Clock = unsafe extern "C" fn(Device, c_int, *mut c_uint) -> Status;
    type Reasons = unsafe extern "C" fn(Device, *mut c_ulonglong) -> Status;
    // SAFETY, for every call below: the types above are nvml.h's, every out pointer is to a
    // live local, and nothing is read from one whose call did not return success.
    unsafe {
        let count: Count = nvml.symbol(c"nvmlDeviceGetCount_v2")?;
        let handle: Handle = nvml.symbol(c"nvmlDeviceGetHandleByIndex_v2")?;
        let pci: Pci = nvml.symbol(c"nvmlDeviceGetPciInfo_v3")?;
        let name: Option<Name> = nvml.symbol(c"nvmlDeviceGetName");
        let usage: Option<Use> = nvml.symbol(c"nvmlDeviceGetUtilizationRates");
        let memory: Option<Mem> = nvml.symbol(c"nvmlDeviceGetMemoryInfo");
        let temperature: Option<Temp> = nvml.symbol(c"nvmlDeviceGetTemperature");
        let power: Option<Power> = nvml.symbol(c"nvmlDeviceGetPowerUsage");
        let clock: Option<Clock> = nvml.symbol(c"nvmlDeviceGetClockInfo");
        let reasons: Option<Reasons> = nvml
            .symbol(c"nvmlDeviceGetCurrentClocksEventReasons")
            .or_else(|| nvml.symbol(c"nvmlDeviceGetCurrentClocksThrottleReasons"));
        let mut n: c_uint = 0;
        if count(&mut n) != SUCCESS {
            return None;
        }
        let mut cards = Vec::new();
        for index in 0..n {
            let mut device: Device = std::ptr::null_mut();
            let mut info: PciInfo = std::mem::zeroed();
            if handle(index, &mut device) != SUCCESS || pci(device, &mut info) != SUCCESS {
                continue;
            }
            let slot = format!("{:04x}:{:02x}:{:02x}.0", info.domain, info.bus, info.device);
            let mut card = GpuMetrics::of(slot, Vendor::Nvidia);
            if let Some(name) = name {
                let mut text = [0 as c_char; 96];
                if name(device, text.as_mut_ptr(), text.len() as c_uint) == SUCCESS {
                    card.name = Some(CStr::from_ptr(text.as_ptr()).to_string_lossy().into_owned());
                }
            }
            if let Some(usage) = usage {
                let mut u = Utilization { gpu: 0, memory: 0 };
                if usage(device, &mut u) == SUCCESS {
                    card.util_pct = Some(u.gpu);
                }
            }
            if let Some(memory) = memory {
                let mut m = Memory {
                    total: 0,
                    free: 0,
                    used: 0,
                };
                if memory(device, &mut m) == SUCCESS {
                    card.mem_used = Some(m.used);
                    card.mem_total = Some(m.total);
                }
            }
            let mut value: c_uint = 0;
            if let Some(temperature) = temperature
                && temperature(device, TEMPERATURE_GPU, &mut value) == SUCCESS
            {
                card.temp_c = Some(value as i32);
            }
            if let Some(power) = power
                && power(device, &mut value) == SUCCESS
            {
                card.power_mw = Some(u64::from(value));
            }
            if let Some(clock) = clock
                && clock(device, CLOCK_SM, &mut value) == SUCCESS
            {
                card.clock_mhz = Some(value);
            }
            let mut bits: c_ulonglong = 0;
            if let Some(reasons) = reasons
                && reasons(device, &mut bits) == SUCCESS
            {
                card.throttle = Some(
                    REASONS
                        .iter()
                        .filter(|(bit, _)| bits & bit != 0)
                        .map(|(_, reason)| (*reason).to_string())
                        .collect(),
                );
            }
            cards.push(card);
        }
        Some(cards)
    }
}
