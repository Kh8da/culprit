//! Reading the kernel's process list (what Task Manager reads) and turning two readings a
//! second apart into each process's CPU share and I/O rate. No process is ever opened.

use std::collections::HashMap;

/// One process in the list.
#[derive(Clone, Debug, PartialEq)]
pub struct RawProc {
    pub pid: u32,
    /// The exe file name, e.g. "cs2.exe"; "System", "Registry" and the like for the kernel's own.
    pub name: String,
    /// When it started, to tell a reused process id from the process that had it before.
    pub created: i64,
    /// User plus kernel CPU time so far, in 100 ns units.
    pub cpu_time: u64,
    /// Bytes read, written and otherwise transferred so far, files and sockets alike.
    pub io_bytes: u64,
}

// Field offsets in SYSTEM_PROCESS_INFORMATION on 64-bit Windows.
const NEXT: usize = 0;
const CREATE_TIME: usize = 32;
const USER_TIME: usize = 40;
const KERNEL_TIME: usize = 48;
const NAME_LENGTH: usize = 56;
const NAME_BUFFER: usize = 64;
const PROCESS_ID: usize = 80;
const READ_TRANSFER: usize = 232;
const WRITE_TRANSFER: usize = 240;
const OTHER_TRANSFER: usize = 248;
const ENTRY_SIZE: usize = 256;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Parses the buffer `NtQuerySystemInformation(SystemProcessInformation)` filled in. Name
/// strings are pointers into the same buffer, so `base` is the buffer's address.
pub fn parse_process_list(buf: &[u8], base: usize) -> Vec<RawProc> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + ENTRY_SIZE <= buf.len() {
        let e = &buf[at..];
        let len = u16_at(e, NAME_LENGTH) as usize;
        let ptr = u64_at(e, NAME_BUFFER) as usize;
        let name = ptr
            .checked_sub(base)
            .filter(|&start| len > 0 && start.checked_add(len).is_some_and(|end| end <= buf.len()))
            .map(|start| {
                let units: Vec<u16> =
                    buf[start..start + len].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                String::from_utf16_lossy(&units)
            })
            .unwrap_or_default();
        out.push(RawProc {
            pid: u64_at(e, PROCESS_ID) as u32,
            name,
            created: u64_at(e, CREATE_TIME) as i64,
            cpu_time: u64_at(e, USER_TIME).saturating_add(u64_at(e, KERNEL_TIME)),
            io_bytes: u64_at(e, READ_TRANSFER)
                .saturating_add(u64_at(e, WRITE_TRANSFER))
                .saturating_add(u64_at(e, OTHER_TRANSFER)),
        });
        let next = u32_at(e, NEXT) as usize;
        if next == 0 {
            break;
        }
        at += next;
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    /// Share of the whole CPU, 0 to 100.
    pub cpu: f32,
    /// Bytes per second.
    pub io: f32,
}

/// Remembers the last reading of each process to work out rates from the next one.
#[derive(Default)]
pub struct ProcRates {
    last: HashMap<u32, (i64, u64, u64)>,
}

impl ProcRates {
    /// Rates for each process in `procs` over the `dt` seconds since the last call, in the
    /// same order. `None` for a process that wasn't in the last reading.
    pub fn update(&mut self, procs: &[RawProc], dt: f64, logical_cpus: u32) -> Vec<Option<Rate>> {
        let cpu_ticks = dt.max(0.001) * 10_000_000.0 * logical_cpus.max(1) as f64;
        let rates = procs
            .iter()
            .map(|p| {
                let &(_, cpu, io) = self.last.get(&p.pid).filter(|l| l.0 == p.created)?;
                Some(Rate {
                    cpu: (p.cpu_time.saturating_sub(cpu) as f64 * 100.0 / cpu_ticks).clamp(0.0, 100.0) as f32,
                    io: (p.io_bytes.saturating_sub(io) as f64 / dt.max(0.001)) as f32,
                })
            })
            .collect();
        self.last = procs.iter().map(|p| (p.pid, (p.created, p.cpu_time, p.io_bytes))).collect();
        rates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a process list buffer the way the kernel lays it out, names after the entries.
    fn buffer(procs: &[RawProc], base: usize) -> Vec<u8> {
        let names_at = procs.len() * ENTRY_SIZE;
        let mut buf = vec![0u8; names_at];
        for (i, p) in procs.iter().enumerate() {
            let name: Vec<u8> = p.name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            let name_ptr = base + buf.len();
            buf.extend_from_slice(&name);
            let e = &mut buf[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE];
            let next = if i + 1 == procs.len() { 0 } else { ENTRY_SIZE as u32 };
            e[NEXT..NEXT + 4].copy_from_slice(&next.to_le_bytes());
            e[CREATE_TIME..CREATE_TIME + 8].copy_from_slice(&p.created.to_le_bytes());
            e[USER_TIME..USER_TIME + 8].copy_from_slice(&(p.cpu_time / 4).to_le_bytes());
            e[KERNEL_TIME..KERNEL_TIME + 8].copy_from_slice(&(p.cpu_time - p.cpu_time / 4).to_le_bytes());
            e[NAME_LENGTH..NAME_LENGTH + 2].copy_from_slice(&(name.len() as u16).to_le_bytes());
            e[NAME_LENGTH + 2..NAME_LENGTH + 4].copy_from_slice(&(name.len() as u16).to_le_bytes());
            let ptr = if name.is_empty() { 0 } else { name_ptr as u64 };
            e[NAME_BUFFER..NAME_BUFFER + 8].copy_from_slice(&ptr.to_le_bytes());
            e[PROCESS_ID..PROCESS_ID + 8].copy_from_slice(&(p.pid as u64).to_le_bytes());
            e[READ_TRANSFER..READ_TRANSFER + 8].copy_from_slice(&(p.io_bytes / 2).to_le_bytes());
            e[WRITE_TRANSFER..WRITE_TRANSFER + 8].copy_from_slice(&(p.io_bytes / 4).to_le_bytes());
            let other = p.io_bytes - p.io_bytes / 2 - p.io_bytes / 4;
            e[OTHER_TRANSFER..OTHER_TRANSFER + 8].copy_from_slice(&other.to_le_bytes());
        }
        buf
    }

    fn proc(pid: u32, name: &str, cpu_time: u64, io_bytes: u64) -> RawProc {
        RawProc { pid, name: name.into(), created: 1000 + pid as i64, cpu_time, io_bytes }
    }

    #[test]
    fn parses_the_kernel_layout() {
        let procs = vec![proc(0, "", 0, 0), proc(4, "System", 77, 9), proc(4120, "cs2.exe", 1_000_003, 123_457)];
        let base = 0x7ff0_0000_0000;
        assert_eq!(parse_process_list(&buffer(&procs, base), base), procs);
        // A truncated buffer or a name pointing outside it doesn't crash.
        let buf = buffer(&procs, base);
        assert_eq!(parse_process_list(&buf[..300], base).len(), 1);
        assert_eq!(parse_process_list(&buf, base + 1_000_000)[2].name, "");
        assert!(parse_process_list(&[], base).is_empty());
    }

    #[test]
    fn rates_from_two_readings() {
        let mut rates = ProcRates::default();
        let first = vec![proc(10, "a.exe", 0, 0), proc(11, "b.exe", 5_000_000, 1_000)];
        assert_eq!(rates.update(&first, 1.0, 8), vec![None, None]);
        // Half a core over one second on an 8-thread CPU is 6.25% of the whole CPU.
        let second = vec![proc(10, "a.exe", 5_000_000, 3_000_000), proc(11, "b.exe", 5_000_000, 1_000)];
        let r = rates.update(&second, 1.0, 8);
        assert_eq!(r[0], Some(Rate { cpu: 6.25, io: 3_000_000.0 }));
        assert_eq!(r[1], Some(Rate { cpu: 0.0, io: 0.0 }));
        // A reused process id starts over.
        let mut reused = second.clone();
        reused[0].created += 1;
        assert_eq!(rates.update(&reused, 1.0, 8)[0], None);
    }
}
