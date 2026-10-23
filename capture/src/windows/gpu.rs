//! The graphics card: which one the game most likely runs on, and its temperature, clock
//! and whether it is holding itself back. NVIDIA cards report all of that through the
//! driver's NVML library; for others Windows' own GPU statistics give temperature and clock.

use super::util::wide;
use crate::gpu::{luid_tag, nvml_throttle};
use culpritcore::model::GpuThrottle;
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Wdk::Graphics::Direct3D::{
    D3DKMTCloseAdapter, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo, D3DKMT_ADAPTER_PERFDATA,
    D3DKMT_CLOSEADAPTER, D3DKMT_NODE_PERFDATA, D3DKMT_OPENADAPTERFROMLUID, D3DKMT_QUERYADAPTERINFO,
    KMTQAITYPE_ADAPTERPERFDATA, KMTQAITYPE_NODEPERFDATA,
};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

const NVIDIA: u32 = 0x10DE;

#[derive(Clone)]
pub struct Adapter {
    pub name: String,
    pub vram_mb: Option<f32>,
    pub luid: LUID,
    pub vendor: u32,
}

impl Adapter {
    /// How performance counters name this adapter.
    pub fn tag(&self) -> String {
        luid_tag(self.luid.HighPart, self.luid.LowPart)
    }
}

/// The GPU with the most memory of its own: on a laptop with two, the one games use.
pub fn primary_adapter() -> Option<Adapter> {
    // SAFETY: COM calls on interfaces this function owns.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut best: Option<(usize, Adapter)> = None;
        for i in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(i) else { break };
            let Ok(desc) = adapter.GetDesc1() else { continue };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            if best.as_ref().is_none_or(|(mem, _)| desc.DedicatedVideoMemory > *mem) {
                let name = super::util::from_wide(&desc.Description).trim().to_string();
                let vram = desc.DedicatedVideoMemory as f64 / 1_048_576.0;
                best = Some((
                    desc.DedicatedVideoMemory,
                    Adapter {
                        name,
                        vram_mb: (vram >= 64.0).then_some(vram as f32),
                        luid: desc.AdapterLuid,
                        vendor: desc.VendorId,
                    },
                ));
            }
        }
        best.map(|(_, a)| a)
    }
}

pub struct Reading {
    pub temp: Option<f32>,
    pub clock: Option<f32>,
    pub throttle: Option<GpuThrottle>,
}

/* ---------- NVIDIA ---------- */

type NvmlDevice = *mut c_void;
type NvmlInit = unsafe extern "C" fn() -> i32;
type NvmlCount = unsafe extern "C" fn(*mut u32) -> i32;
type NvmlByIndex = unsafe extern "C" fn(u32, *mut NvmlDevice) -> i32;
type NvmlName = unsafe extern "C" fn(NvmlDevice, *mut u8, u32) -> i32;
type NvmlU32Query = unsafe extern "C" fn(NvmlDevice, u32, *mut u32) -> i32;
type NvmlReasons = unsafe extern "C" fn(NvmlDevice, *mut u64) -> i32;

struct Nvml {
    device: NvmlDevice,
    temperature: NvmlU32Query,
    clock: NvmlU32Query,
    reasons: Option<NvmlReasons>,
    shutdown: NvmlInit,
}

impl Nvml {
    /// Loads the driver's NVML and finds the device named like `adapter`.
    fn open(adapter: &Adapter) -> Option<Nvml> {
        // SAFETY: nvml.dll is loaded from System32 only (or NVIDIA's own folder on old
        // drivers); each function is cast to its documented signature.
        unsafe {
            let lib = LoadLibraryExW(PCWSTR(wide("nvml.dll").as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
                .or_else(|_| {
                    let old = wide(r"C:\Program Files\NVIDIA Corporation\NVSMI\nvml.dll");
                    LoadLibraryExW(PCWSTR(old.as_ptr()), None, Default::default())
                })
                .ok()?;
            let get = |name: &str| -> Option<*const c_void> {
                let name = format!("{name}\0");
                GetProcAddress(lib, windows::core::PCSTR(name.as_ptr())).map(|f| f as *const c_void)
            };
            let init: NvmlInit = std::mem::transmute(get("nvmlInit_v2")?);
            let shutdown: NvmlInit = std::mem::transmute(get("nvmlShutdown")?);
            let count: NvmlCount = std::mem::transmute(get("nvmlDeviceGetCount_v2")?);
            let by_index: NvmlByIndex = std::mem::transmute(get("nvmlDeviceGetHandleByIndex_v2")?);
            let name: NvmlName = std::mem::transmute(get("nvmlDeviceGetName")?);
            let temperature: NvmlU32Query = std::mem::transmute(get("nvmlDeviceGetTemperature")?);
            let clock: NvmlU32Query = std::mem::transmute(get("nvmlDeviceGetClockInfo")?);
            let reasons = get("nvmlDeviceGetCurrentClocksEventReasons")
                .or_else(|| get("nvmlDeviceGetCurrentClocksThrottleReasons"))
                .map(|f| std::mem::transmute::<*const c_void, NvmlReasons>(f));
            if init() != 0 {
                return None;
            }
            let mut n = 0u32;
            let mut chosen = None;
            if count(&mut n) == 0 {
                for i in 0..n {
                    let mut device = std::ptr::null_mut();
                    if by_index(i, &mut device) != 0 {
                        continue;
                    }
                    let mut buf = [0u8; 96];
                    let named = name(device, buf.as_mut_ptr(), buf.len() as u32) == 0;
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    let device_name = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                    let matches = named && adapter.name.to_ascii_lowercase().contains(device_name.trim());
                    if chosen.is_none() || matches {
                        chosen = Some(device);
                    }
                    if matches {
                        break;
                    }
                }
            }
            let Some(device) = chosen else {
                shutdown();
                return None;
            };
            Some(Nvml { device, temperature, clock, reasons, shutdown })
        }
    }

    fn read(&self) -> Reading {
        let (mut temp, mut clock, mut reasons) = (0u32, 0u32, 0u64);
        // SAFETY: the device handle stays valid until nvmlShutdown in drop.
        unsafe {
            let temp_ok = (self.temperature)(self.device, 0, &mut temp) == 0; // NVML_TEMPERATURE_GPU
            let clock_ok = (self.clock)(self.device, 0, &mut clock) == 0; // NVML_CLOCK_GRAPHICS
            let reasons_ok = self.reasons.is_some_and(|f| f(self.device, &mut reasons) == 0);
            Reading {
                temp: temp_ok.then_some(temp as f32),
                clock: (clock_ok && clock > 0).then_some(clock as f32),
                throttle: reasons_ok.then(|| nvml_throttle(reasons)),
            }
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: balances the nvmlInit_v2 in open.
        unsafe { (self.shutdown)() };
    }
}

/* ---------- any vendor, through Windows ---------- */

struct Kmt {
    adapter: u32,
}

impl Kmt {
    fn open(luid: LUID) -> Option<Kmt> {
        let mut open = D3DKMT_OPENADAPTERFROMLUID { AdapterLuid: luid, hAdapter: 0 };
        // SAFETY: plain in/out structure.
        unsafe { D3DKMTOpenAdapterFromLuid(&mut open) }.is_ok().then_some(Kmt { adapter: open.hAdapter })
    }

    fn query<T>(&self, kind: windows::Wdk::Graphics::Direct3D::KMTQUERYADAPTERINFOTYPE, data: &mut T) -> bool {
        let mut q = D3DKMT_QUERYADAPTERINFO {
            hAdapter: self.adapter,
            Type: kind,
            pPrivateDriverData: (data as *mut T).cast(),
            PrivateDriverDataSize: std::mem::size_of::<T>() as u32,
        };
        // SAFETY: the driver writes at most PrivateDriverDataSize bytes into `data`.
        unsafe { D3DKMTQueryAdapterInfo(&mut q) }.is_ok()
    }

    fn read(&self) -> Reading {
        let mut perf = D3DKMT_ADAPTER_PERFDATA::default();
        let mut node = D3DKMT_NODE_PERFDATA::default();
        // Temperature is in tenths of a degree; node 0 is the 3D engine, its clock in Hz.
        let temp = (self.query(KMTQAITYPE_ADAPTERPERFDATA, &mut perf) && perf.Temperature > 0)
            .then(|| perf.Temperature as f32 / 10.0);
        let clock = (self.query(KMTQAITYPE_NODEPERFDATA, &mut node) && node.Frequency > 0)
            .then(|| (node.Frequency as f64 / 1_000_000.0) as f32);
        Reading { temp, clock, throttle: None }
    }
}

impl Drop for Kmt {
    fn drop(&mut self) {
        let close = D3DKMT_CLOSEADAPTER { hAdapter: self.adapter };
        // SAFETY: closes the adapter handle this value opened.
        let _ = unsafe { D3DKMTCloseAdapter(&close) };
    }
}

/* ---------- whichever works ---------- */

pub struct Sensors {
    nvml: Option<Nvml>,
    kmt: Option<Kmt>,
}

// SAFETY: NVML and D3DKMT handles may be used from any thread; the sampler uses them from
// one at a time.
unsafe impl Send for Sensors {}

impl Sensors {
    pub fn new(adapter: Option<&Adapter>) -> Self {
        let nvml = adapter.filter(|a| a.vendor == NVIDIA).and_then(Nvml::open);
        let kmt = if nvml.is_none() { adapter.and_then(|a| Kmt::open(a.luid)) } else { None };
        Sensors { nvml, kmt }
    }

    pub fn read(&mut self) -> Reading {
        if let Some(n) = &self.nvml {
            return n.read();
        }
        if let Some(k) = &self.kmt {
            return k.read();
        }
        Reading { temp: None, clock: None, throttle: None }
    }
}
