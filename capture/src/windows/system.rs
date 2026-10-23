//! Sampling the whole PC once a second, the way Task Manager does: CPU, memory, disk, GPU
//! and every process. Processes are read from the kernel's process list, so none of them,
//! the game included, is ever opened.

use super::gpu::{Adapter, Sensors};
use super::net::Wifi;
use super::util::{from_wide, wide};
use crate::collector::ProcReading;
use crate::naming::{self, Service};
use crate::procs::{parse_process_list, ProcRates, RawProc};
use culpritcore::model::{Machine, ProcessKind, SystemSample};
use culpritcore::names;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use windows::core::{GUID, PCWSTR};
use windows::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemProcessInformation, SYSTEM_INFORMATION_CLASS,
};
use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, STATUS_INFO_LENGTH_MISMATCH};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, GetLogicalDriveStringsW, QueryDosDeviceW, VerQueryValueW,
};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT,
    PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW, SC_ENUM_PROCESS_INFO,
    SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE, SERVICE_WIN32,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{GetActiveProcessorCount, GetSystemTimes, ALL_PROCESSOR_GROUPS};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, InternalGetWindowText,
    IsWindowVisible,
};

const MB: f64 = 1_048_576.0;

/* ---------- the machine ---------- */

fn registry_string(key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut buf = vec![0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: `size` is the buffer's size in bytes; strings outlive the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    status.is_ok().then(|| from_wide(&buf).trim().to_string()).filter(|s| !s.is_empty())
}

fn memory() -> Option<MEMORYSTATUSEX> {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: dwLength is set as the call requires.
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok().map(|_| m)
}

pub fn machine(gpu: Option<&Adapter>) -> Machine {
    let version = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = registry_string(version, "CurrentBuildNumber").and_then(|b| b.parse().ok()).unwrap_or(0);
    Machine {
        cpu: registry_string(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString"),
        // SAFETY: no arguments to get wrong.
        logical_cpus: unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.max(1),
        mem_total_mb: memory().map(|m| (m.ullTotalPhys as f64 / MB) as f32),
        gpu: gpu.map(|g| g.name.clone()),
        vram_total_mb: gpu.and_then(|g| g.vram_mb),
        os: Some(naming::os_name(build, registry_string(version, "DisplayVersion").as_deref())),
    }
}

/* ---------- windows and program files ---------- */

unsafe extern "system" fn find_child_app(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    // SAFETY: lparam points at the (host pid, found pid) pair owned by the caller.
    let pids = unsafe { &mut *(lparam.0 as *mut (u32, u32)) };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != 0 && pid != pids.0 {
        pids.1 = pid;
        return false.into();
    }
    true.into()
}

/// Title of a visible window of `pid`, preferring the one in front.
pub fn window_title(pid: u32) -> Option<String> {
    let title = |hwnd: HWND| {
        let mut buf = [0u16; 256];
        // SAFETY: InternalGetWindowText never sends messages to the window's program.
        let n = unsafe { InternalGetWindowText(hwnd, &mut buf) }.max(0) as usize;
        Some(String::from_utf16_lossy(&buf[..n.min(buf.len())])).filter(|t| !t.trim().is_empty())
    };
    // SAFETY: plain window queries.
    unsafe {
        let front = GetForegroundWindow();
        let mut owner = 0;
        GetWindowThreadProcessId(front, Some(&mut owner));
        if owner == pid {
            if let Some(t) = title(front) {
                return Some(t);
            }
        }
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        // SAFETY: lparam points at the (pid, found window) pair owned by the caller.
        let wanted = unsafe { &mut *(lparam.0 as *mut (u32, HWND)) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == wanted.0 && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            let mut buf = [0u16; 2];
            if unsafe { InternalGetWindowText(hwnd, &mut buf) } > 0 {
                wanted.1 = hwnd;
                return false.into();
            }
        }
        true.into()
    }
    let mut found = (pid, HWND::default());
    // SAFETY: `found` outlives the enumeration.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut found as *mut _ as isize)) };
    (!found.1 .0.is_null()).then(|| title(found.1)).flatten()
}

/// The NT path of a process's exe, from the process list (no handle to the process).
fn image_path(pid: u32) -> Option<String> {
    #[repr(C)]
    struct ProcessIdInfo {
        pid: usize,
        length: u16,
        max_length: u16,
        buffer: *mut u16,
    }
    const SYSTEM_PROCESS_ID_INFORMATION: SYSTEM_INFORMATION_CLASS = SYSTEM_INFORMATION_CLASS(88);
    // Room for any normal path first, then for the longest path Windows allows.
    for chars in [1024usize, 32_767] {
        let mut buf = vec![0u16; chars];
        let mut info =
            ProcessIdInfo { pid: pid as usize, length: 0, max_length: (chars * 2) as u16, buffer: buf.as_mut_ptr() };
        // SAFETY: the kernel writes at most max_length bytes into `buf`.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_ID_INFORMATION,
                (&mut info as *mut ProcessIdInfo).cast(),
                std::mem::size_of::<ProcessIdInfo>() as u32,
                std::ptr::null_mut(),
            )
        };
        if status.is_ok() {
            let len = (info.length as usize / 2).min(buf.len());
            return Some(String::from_utf16_lossy(&buf[..len]));
        }
        if status != STATUS_INFO_LENGTH_MISMATCH {
            return None;
        }
    }
    None
}

/// "\Device\HarddiskVolume3" to "C:" for every drive letter.
fn dos_devices() -> Vec<(String, String)> {
    let mut drives = vec![0u16; 512];
    // SAFETY: the buffer length is passed with the slice.
    let n = unsafe { GetLogicalDriveStringsW(Some(&mut drives)) } as usize;
    drives[..n.min(drives.len())]
        .split(|&c| c == 0)
        .filter(|d| d.len() >= 2)
        .filter_map(|d| {
            let letter = String::from_utf16_lossy(&d[..2]);
            let mut target = vec![0u16; 512];
            // SAFETY: as above.
            let n = unsafe { QueryDosDeviceW(PCWSTR(wide(&letter).as_ptr()), Some(&mut target)) };
            (n > 0).then(|| (from_wide(&target), letter))
        })
        .collect()
}

/// FileDescription and ProductName from an exe's version resource.
fn version_info(path: &str) -> (Option<String>, Option<String>) {
    let file = wide(path);
    // SAFETY: the buffer is sized as GetFileVersionInfoSizeW asks; VerQueryValueW hands back
    // pointers into it, read with the lengths it gives.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(file.as_ptr()), None);
        if size == 0 {
            return (None, None);
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(PCWSTR(file.as_ptr()), None, size, data.as_mut_ptr().cast()).is_err() {
            return (None, None);
        }
        let query = |sub: &str| -> Option<(*const u8, usize)> {
            let sub = wide(sub);
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            VerQueryValueW(data.as_ptr().cast(), PCWSTR(sub.as_ptr()), &mut ptr, &mut len)
                .as_bool()
                .then_some((ptr as *const u8, len as usize))
                .filter(|(p, l)| !p.is_null() && *l > 0)
        };
        let mut langs = vec![(0x0409u16, 0x04B0u16), (0x0409, 0x04E4), (0x0000, 0x04B0)];
        if let Some((p, _)) = query(r"\VarFileInfo\Translation").filter(|(_, l)| *l >= 4) {
            let pair = std::slice::from_raw_parts(p as *const u16, 2);
            langs.insert(0, (pair[0], pair[1]));
        }
        let string = |name: &str| {
            langs.iter().find_map(|(lang, cp)| {
                let (p, len) = query(&format!(r"\StringFileInfo\{lang:04x}{cp:04x}\{name}"))?;
                Some(from_wide(std::slice::from_raw_parts(p as *const u16, len))).filter(|s| !s.trim().is_empty())
            })
        };
        (string("FileDescription"), string("ProductName"))
    }
}

/* ---------- services ---------- */

fn services() -> HashMap<u32, Vec<Service>> {
    let mut out: HashMap<u32, Vec<Service>> = HashMap::new();
    // SAFETY: the buffer is u64-aligned and sized as the first call asks; entries and their
    // strings live inside it.
    unsafe {
        let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE) else { return out };
        let mut needed = 0u32;
        let mut count = 0u32;
        let mut resume = 0u32;
        let _ = EnumServicesStatusExW(
            scm,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_ACTIVE,
            None,
            &mut needed,
            &mut count,
            Some(&mut resume),
            PCWSTR::null(),
        );
        let mut buf = vec![0u64; needed as usize / 8 + 1024];
        let bytes = std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8);
        resume = 0;
        let ok = EnumServicesStatusExW(
            scm,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_ACTIVE,
            Some(bytes),
            &mut needed,
            &mut count,
            Some(&mut resume),
            PCWSTR::null(),
        )
        .is_ok();
        if ok {
            let items = std::slice::from_raw_parts(buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW, count as usize);
            for s in items {
                let pid = s.ServiceStatusProcess.dwProcessId;
                if pid != 0 {
                    out.entry(pid).or_default().push(Service {
                        name: s.lpServiceName.to_string().unwrap_or_default(),
                        display: s.lpDisplayName.to_string().unwrap_or_default(),
                    });
                }
            }
        }
        let _ = CloseServiceHandle(scm);
    }
    out
}

/* ---------- performance counters ---------- */

const FMT: PDH_FMT = PDH_FMT(0x0000_0200 | 0x0000_8000); // double, not capped at 100

struct Pdh {
    query: PDH_HQUERY,
    cpu_perf: Option<PDH_HCOUNTER>,
    pages_in: Option<PDH_HCOUNTER>,
    disk_idle: Option<PDH_HCOUNTER>,
    gpu_engine: Option<PDH_HCOUNTER>,
    gpu_memory: Option<PDH_HCOUNTER>,
    items: Vec<u64>,
}

// SAFETY: PDH handles may be used from any thread; the sampler uses them from one at a time.
unsafe impl Send for Pdh {}

impl Pdh {
    fn open() -> Option<Pdh> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: plain out-parameter.
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
            return None;
        }
        let add = |path: &str| {
            let mut c = PDH_HCOUNTER::default();
            let path = wide(path);
            // SAFETY: the path outlives the call.
            (unsafe { PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut c) } == 0).then_some(c)
        };
        let pdh = Pdh {
            query,
            cpu_perf: add(r"\Processor Information(_Total)\% Processor Performance"),
            pages_in: add(r"\Memory\Pages Input/sec"),
            disk_idle: add(r"\PhysicalDisk(_Total)\% Idle Time"),
            gpu_engine: add(r"\GPU Engine(*)\Utilization Percentage"),
            gpu_memory: add(r"\GPU Adapter Memory(*)\Dedicated Usage"),
            items: Vec::new(),
        };
        // Rates need two readings; take the first now.
        // SAFETY: valid query handle.
        unsafe { PdhCollectQueryData(query) };
        Some(pdh)
    }

    fn collect(&self) {
        // SAFETY: valid query handle.
        unsafe { PdhCollectQueryData(self.query) };
    }

    fn value(&self, counter: Option<PDH_HCOUNTER>) -> Option<f64> {
        let mut v = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: plain out-parameter; the union is read as the double that was asked for.
        unsafe {
            (PdhGetFormattedCounterValue(counter?, FMT, None, &mut v) == 0
                && (v.CStatus == PDH_CSTATUS_VALID_DATA || v.CStatus == PDH_CSTATUS_NEW_DATA))
                .then_some(v.Anonymous.doubleValue)
                .filter(|x| x.is_finite())
        }
    }

    /// Every instance of a wildcard counter, by instance name.
    fn values(&mut self, counter: Option<PDH_HCOUNTER>) -> Vec<(String, f64)> {
        let Some(counter) = counter else { return Vec::new() };
        let mut size = 0u32;
        let mut count = 0u32;
        // SAFETY: the first call only reports the size; the second writes into a buffer of
        // that size (u64-aligned), whose items and names are read before it is reused.
        unsafe {
            if PdhGetFormattedCounterArrayW(counter, FMT, &mut size, &mut count, None) != PDH_MORE_DATA {
                return Vec::new();
            }
            self.items.resize(size as usize / 8 + 1, 0);
            let items = self.items.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(counter, FMT, &mut size, &mut count, Some(items)) != 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts(items, count as usize)
                .iter()
                .filter(|i| i.FmtValue.CStatus == PDH_CSTATUS_VALID_DATA || i.FmtValue.CStatus == PDH_CSTATUS_NEW_DATA)
                .map(|i| (i.szName.to_string().unwrap_or_default(), i.FmtValue.Anonymous.doubleValue))
                .collect()
        }
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        // SAFETY: closes the query this value opened.
        unsafe { PdhCloseQuery(self.query) };
    }
}

/* ---------- the sampler ---------- */

fn filetime(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// (idle, kernel + user) CPU time across all cores, in 100 ns units.
fn cpu_times() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: plain out-parameters.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    Some((filetime(idle), filetime(kernel) + filetime(user)))
}

pub struct Sampler {
    cpus: u32,
    cpu_last: Option<(u64, u64)>,
    pdh: Option<Pdh>,
    list: Vec<u64>,
    rates: ProcRates,
    /// Exe name by process id, from the last reading.
    names: HashMap<u32, String>,
    /// Grouping key, label and kind by process id and start time.
    labels: HashMap<(u32, i64), (String, String, ProcessKind)>,
    descriptions: HashMap<String, Option<String>>,
    services: HashMap<u32, Vec<Service>>,
    services_read: Instant,
    devices: Vec<(String, String)>,
    adapter: Option<String>,
    sensors: Sensors,
    wifi: Option<Wifi>,
    notes: Vec<String>,
}

impl Sampler {
    pub fn new(cpus: u32, gpu: Option<Adapter>, wifi: Option<GUID>) -> Self {
        let sensors = Sensors::new(gpu.as_ref());
        let mut s = Sampler {
            cpus,
            cpu_last: cpu_times(),
            pdh: Pdh::open(),
            list: vec![0u64; 64 * 1024],
            rates: ProcRates::default(),
            names: HashMap::new(),
            labels: HashMap::new(),
            descriptions: HashMap::new(),
            services: services(),
            services_read: Instant::now(),
            devices: dos_devices(),
            adapter: gpu.map(|g| g.tag()),
            sensors,
            wifi: None,
            notes: Vec::new(),
        };
        s.set_wifi(wifi);
        s
    }

    /// Takes the readings the first second's rates are measured from.
    pub fn prime(&mut self) {
        self.cpu_last = cpu_times();
        let procs = self.processes();
        self.rates.update(&procs, 1.0, self.cpus);
        self.names = procs.iter().map(|p| (p.pid, p.name.clone())).collect();
        if let Some(pdh) = &self.pdh {
            pdh.collect();
        }
    }

    /// Things that couldn't be measured, to put in the report's notes.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    pub fn set_wifi(&mut self, guid: Option<GUID>) {
        self.wifi = guid.and_then(Wifi::open);
    }

    /// The process that owns the window in front.
    pub fn foreground(&self) -> Option<u32> {
        // SAFETY: plain window queries; the callback's lparam points at a local.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return None;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            // Store apps draw in a child window of the frame host; the child's owner is the app.
            if self.names.get(&pid).is_some_and(|n| n.eq_ignore_ascii_case("ApplicationFrameHost.exe")) {
                let mut pids = (pid, 0u32);
                let _ = EnumChildWindows(Some(hwnd), Some(find_child_app), LPARAM(&mut pids as *mut _ as isize));
                if pids.1 != 0 {
                    return Some(pids.1);
                }
            }
            (pid != 0).then_some(pid)
        }
    }

    fn processes(&mut self) -> Vec<RawProc> {
        for _ in 0..4 {
            let mut needed = 0u32;
            let len = (self.list.len() * 8) as u32;
            // SAFETY: the kernel writes at most `len` bytes into the u64-aligned buffer.
            let status = unsafe {
                NtQuerySystemInformation(SystemProcessInformation, self.list.as_mut_ptr().cast(), len, &mut needed)
            };
            if status.is_ok() {
                // SAFETY: the buffer is plain bytes the kernel just filled.
                let bytes = unsafe { std::slice::from_raw_parts(self.list.as_ptr() as *const u8, self.list.len() * 8) };
                return parse_process_list(bytes, self.list.as_ptr() as usize);
            }
            if status != STATUS_INFO_LENGTH_MISMATCH {
                break;
            }
            self.list.resize((needed as usize).max(len as usize) / 8 + 8 * 1024, 0);
        }
        Vec::new()
    }

    /// The drive-letter path of a process's exe.
    fn exe_path(&self, pid: u32) -> Option<String> {
        let nt = image_path(pid)?;
        self.devices.iter().find_map(|(device, letter)| {
            let rest = nt.get(device.len()..).filter(|r| r.starts_with('\\'))?;
            nt[..device.len()].eq_ignore_ascii_case(device).then(|| format!("{letter}{rest}"))
        })
    }

    /// The game's own description and product name, for naming it.
    pub fn version_strings(&self, pid: u32) -> (Option<String>, Option<String>) {
        self.exe_path(pid).map(|p| version_info(&p)).unwrap_or((None, None))
    }

    fn identify(&mut self, p: &RawProc) -> (String, String, ProcessKind) {
        if let Some(found) = self.labels.get(&(p.pid, p.created)) {
            return found.clone();
        }
        let is_host = p.name.eq_ignore_ascii_case("svchost.exe");
        if is_host && !self.services.contains_key(&p.pid) && self.services_read.elapsed() > Duration::from_secs(5) {
            self.services = services();
            self.services_read = Instant::now();
        }
        let services = self.services.get(&p.pid).cloned().unwrap_or_default();
        // Only read the exe file when nothing better names the program; once per exe.
        let description = if names::app_label(&p.name).is_none() && !(is_host && !services.is_empty()) {
            self.exe_path(p.pid).and_then(|path| {
                self.descriptions.entry(path.to_ascii_lowercase()).or_insert_with(|| version_info(&path).0).clone()
            })
        } else {
            None
        };
        let id = naming::identify(&p.name, &services, || description);
        // A service host that starts a service later is looked at again.
        if !(is_host && services.is_empty()) {
            self.labels.insert((p.pid, p.created), id.clone());
        }
        id
    }

    /// Whole-PC numbers and every process's CPU and I/O over the last `dt` seconds.
    pub fn sample(&mut self, dt: f64) -> (SystemSample, Vec<ProcReading>) {
        let mut s = SystemSample::default();
        if let Some((idle, total)) = cpu_times() {
            if let Some((idle0, total0)) = self.cpu_last {
                let busy = total.saturating_sub(total0).saturating_sub(idle.saturating_sub(idle0));
                let all = total.saturating_sub(total0);
                if all > 0 {
                    s.cpu = Some((busy as f64 * 100.0 / all as f64).clamp(0.0, 100.0) as f32);
                }
            }
            self.cpu_last = Some((idle, total));
        }
        s.mem_free_mb = memory().map(|m| (m.ullAvailPhys as f64 / MB) as f32);
        if let Some(mut pdh) = self.pdh.take() {
            pdh.collect();
            s.cpu_perf = pdh.value(pdh.cpu_perf).map(|v| v as f32);
            s.hard_faults = pdh.value(pdh.pages_in).map(|v| v as f32);
            s.disk_busy = pdh.value(pdh.disk_idle).map(|idle| (100.0 - idle).clamp(0.0, 100.0) as f32);
            let adapter = self.adapter.as_deref();
            let engines = pdh.values(pdh.gpu_engine);
            s.gpu = crate::gpu::engine_3d_busy(engines.iter().map(|(n, v)| (n.as_str(), *v)), adapter);
            let memory = pdh.values(pdh.gpu_memory);
            s.vram_used_mb = crate::gpu::adapter_memory_mb(memory.iter().map(|(n, v)| (n.as_str(), *v)), adapter);
            self.pdh = Some(pdh);
        }
        let reading = self.sensors.read();
        s.gpu_temp = reading.temp;
        s.gpu_clock = reading.clock;
        s.gpu_throttle = reading.throttle;
        if let Some(wifi) = self.wifi.as_mut() {
            s.wifi_signal = wifi.signal();
            if let Some(note) = wifi.take_note() {
                self.notes.push(note);
            }
        }

        let procs = self.processes();
        let rates = self.rates.update(&procs, dt, self.cpus);
        let mut readings = Vec::with_capacity(procs.len());
        for (p, rate) in procs.iter().zip(rates) {
            let Some(rate) = rate.filter(|_| p.pid != 0) else { continue };
            let (key, label, kind) = self.identify(p);
            readings.push(ProcReading {
                pid: p.pid,
                exe: p.name.clone(),
                key,
                label,
                kind,
                cpu: rate.cpu,
                io: rate.io,
            });
        }
        self.names = procs.iter().map(|p| (p.pid, p.name.clone())).collect();
        let alive: HashSet<(u32, i64)> = procs.iter().map(|p| (p.pid, p.created)).collect();
        self.labels.retain(|k, _| alive.contains(k));
        (s, readings)
    }
}
