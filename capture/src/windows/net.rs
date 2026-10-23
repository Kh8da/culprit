//! The connection: which adapter and router the PC uses, the Wi-Fi signal, and pings. The
//! only packets Culprit ever sends are these pings (ICMP echo requests), including the
//! traceroute that finds the provider's first hop, which is pings with a hop limit.

use super::util::{from_wide, qpc, qpc_freq, Owned};
use super::{Counts, Shared};
use crate::log;
use crate::targets::{refine_rtt, wifi_band, wifi_quality, Route};
use culpritcore::model::{Connection, LinkKind};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;
use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_IO_PENDING, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0};
use windows::Win32::NetworkManagement::IpHelper::{
    GetBestRoute2, GetIfEntry2, Icmp6CreateFile, Icmp6ParseReplies, Icmp6SendEcho2, IcmpCloseHandle, IcmpCreateFile,
    IcmpParseReplies, IcmpSendEcho2, ICMPV6_ECHO_REPLY_LH, ICMP_ECHO_REPLY, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
    IP_OPTION_INFORMATION, IP_SUCCESS, IP_TTL_EXPIRED_TRANSIT, MIB_IF_ROW2, MIB_IPFORWARD_ROW2,
};
use windows::Win32::NetworkManagement::WiFi::{
    wlan_intf_opcode_channel_number, wlan_intf_opcode_rssi, WlanCloseHandle, WlanFreeMemory, WlanOpenHandle,
    WlanQueryInterface, WLAN_API_VERSION_2_0, WLAN_INTF_OPCODE,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6, SOCKADDR_IN6, SOCKADDR_INET};
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects, WaitForSingleObject};

/* ---------- addresses ---------- */

fn sockaddr(ip: IpAddr) -> SOCKADDR_INET {
    let mut sa = SOCKADDR_INET::default();
    match ip {
        IpAddr::V4(v4) => {
            sa.Ipv4.sin_family = AF_INET;
            sa.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(v4.octets());
        }
        IpAddr::V6(v6) => {
            sa.Ipv6.sin6_family = AF_INET6;
            sa.Ipv6.sin6_addr.u.Byte = v6.octets();
        }
    }
    sa
}

fn ip_of(sa: &SOCKADDR_INET) -> Option<IpAddr> {
    // SAFETY: the family says which member of the union is filled in.
    unsafe {
        match sa.si_family {
            AF_INET => Some(IpAddr::V4(Ipv4Addr::from(sa.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()))),
            AF_INET6 => Some(IpAddr::V6(Ipv6Addr::from(sa.Ipv6.sin6_addr.u.Byte))),
            _ => None,
        }
    }
}

fn v6_words(words: [u16; 8]) -> Ipv6Addr {
    let mut bytes = [0u8; 16];
    for (i, w) in words.iter().enumerate() {
        bytes[i * 2..i * 2 + 2].copy_from_slice(&w.to_ne_bytes());
    }
    Ipv6Addr::from(bytes)
}

/* ---------- the link ---------- */

pub struct Link {
    pub connection: Connection,
    /// The router: the next hop toward the internet.
    pub gateway: Option<IpAddr>,
    /// The Wi-Fi adapter, when the PC is on Wi-Fi.
    pub wifi: Option<GUID>,
}

/// The interface and next hop Windows would use to reach the internet. Looking this up
/// sends nothing.
fn best_route(dest: IpAddr) -> Option<(u32, Option<IpAddr>)> {
    let dest = sockaddr(dest);
    let mut row = MIB_IPFORWARD_ROW2::default();
    let mut source = SOCKADDR_INET::default();
    // SAFETY: plain in/out structures.
    let status = unsafe { GetBestRoute2(None, 0, None, &dest, 0, &mut row, &mut source) };
    (status == ERROR_SUCCESS).then(|| (row.InterfaceIndex, ip_of(&row.NextHop).filter(|ip| !ip.is_unspecified())))
}

pub fn link() -> Link {
    let route = best_route(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)))
        .or_else(|| best_route(IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888))));
    let Some((index, gateway)) = route else {
        return Link { connection: Connection::default(), gateway: None, wifi: None };
    };
    let mut row = MIB_IF_ROW2 { InterfaceIndex: index, ..Default::default() };
    // SAFETY: the row names the interface by index, as the call requires.
    if unsafe { GetIfEntry2(&mut row) } != ERROR_SUCCESS {
        return Link { connection: Connection::default(), gateway, wifi: None };
    }
    let kind = match row.Type {
        IF_TYPE_IEEE80211 => LinkKind::Wifi,
        IF_TYPE_ETHERNET_CSMACD => LinkKind::Ethernet,
        _ => LinkKind::Other,
    };
    let wifi = (kind == LinkKind::Wifi).then_some(row.InterfaceGuid);
    let speed = row.ReceiveLinkSpeed;
    Link {
        connection: Connection {
            kind,
            adapter: Some(from_wide(&row.Description)).filter(|d| !d.is_empty()),
            wifi_band_ghz: wifi.and_then(Wifi::open).and_then(|mut w| w.band()),
            link_mbps: (speed > 0 && speed != u64::MAX).then(|| (speed as f64 / 1_000_000.0) as f32),
        },
        gateway,
        wifi,
    }
}

/* ---------- Wi-Fi ---------- */

/// Reads the Wi-Fi signal. Only the signal strength and channel are asked for: never a scan
/// (which itself causes lag) and never the network list.
pub struct Wifi {
    handle: HANDLE,
    guid: GUID,
    denied: bool,
    note: Option<String>,
}

// SAFETY: WLAN client handles may be used from any thread.
unsafe impl Send for Wifi {}

impl Wifi {
    pub fn open(guid: GUID) -> Option<Wifi> {
        let mut version = 0u32;
        let mut handle = HANDLE::default();
        // SAFETY: plain out-parameters.
        let status = unsafe { WlanOpenHandle(WLAN_API_VERSION_2_0, None, &mut version, &mut handle) };
        (status == ERROR_SUCCESS.0).then_some(Wifi { handle, guid, denied: false, note: None })
    }

    fn query(&self, opcode: WLAN_INTF_OPCODE) -> Result<i32, u32> {
        let mut size = 0u32;
        let mut data = std::ptr::null_mut();
        // SAFETY: WLAN allocates the result, which is read as the 32-bit value these opcodes
        // return and then freed.
        unsafe {
            let status = WlanQueryInterface(self.handle, &self.guid, opcode, None, &mut size, &mut data, None);
            if status != ERROR_SUCCESS.0 {
                return Err(status);
            }
            let value = if size >= 4 && !data.is_null() { Ok(*(data as *const i32)) } else { Err(0) };
            WlanFreeMemory(data);
            value
        }
    }

    /// Signal quality, 0 to 100.
    pub fn signal(&mut self) -> Option<f32> {
        if self.denied {
            return None;
        }
        match self.query(wlan_intf_opcode_rssi) {
            Ok(rssi) if rssi < 0 => Some(wifi_quality(rssi)),
            Ok(_) => None,
            Err(e) => {
                if e == ERROR_ACCESS_DENIED.0 {
                    self.denied = true;
                    self.note = Some("Windows didn't let Culprit read your Wi-Fi signal. Allowing location access for desktop apps (Settings > Privacy & security > Location) lets it.".into());
                }
                None
            }
        }
    }

    pub fn band(&mut self) -> Option<f32> {
        self.query(wlan_intf_opcode_channel_number).ok().and_then(|c| wifi_band(c as u32))
    }

    pub fn take_note(&mut self) -> Option<String> {
        self.note.take()
    }
}

impl Drop for Wifi {
    fn drop(&mut self) {
        // SAFETY: closes the handle this value opened.
        unsafe { WlanCloseHandle(self.handle, None) };
    }
}

/* ---------- pings ---------- */

const PAYLOAD: &[u8; 32] = b"culprit ping: is the line slow? ";
const REPLY_BYTES: usize = 512;
const TIMEOUT_MS: u32 = 1_000;
const EVERY_MS: f64 = 250.0;

struct Icmp {
    v4: Option<HANDLE>,
    v6: Option<HANDLE>,
}

impl Icmp {
    fn open() -> Icmp {
        // SAFETY: the handles are closed in drop.
        unsafe { Icmp { v4: IcmpCreateFile().ok(), v6: Icmp6CreateFile().ok() } }
    }

    /// Sends one echo request. With `event` it returns at once and the event is set when the
    /// reply (or the timeout) lands in `reply`; without, it waits for it.
    ///
    /// # Safety
    /// `reply` must stay in place until the event is set.
    unsafe fn send(&self, ip: IpAddr, event: Option<HANDLE>, ttl: Option<u8>, reply: &mut [u64]) -> bool {
        let options = ttl.map(|t| IP_OPTION_INFORMATION { Ttl: t, ..Default::default() });
        let opts = options.as_ref().map(|o| o as *const _);
        let size = (reply.len() * 8) as u32;
        let sent = match ip {
            IpAddr::V4(v4) => {
                let Some(h) = self.v4 else { return false };
                let dest = u32::from_ne_bytes(v4.octets());
                unsafe {
                    IcmpSendEcho2(
                        h,
                        event,
                        None,
                        None,
                        dest,
                        PAYLOAD.as_ptr().cast(),
                        32,
                        opts,
                        reply.as_mut_ptr().cast(),
                        size,
                        TIMEOUT_MS,
                    )
                }
            }
            IpAddr::V6(v6) => {
                let Some(h) = self.v6 else { return false };
                let source = SOCKADDR_IN6 { sin6_family: AF_INET6, ..Default::default() };
                let mut dest = SOCKADDR_IN6 { sin6_family: AF_INET6, ..Default::default() };
                dest.sin6_addr.u.Byte = v6.octets();
                unsafe {
                    Icmp6SendEcho2(
                        h,
                        event,
                        None,
                        None,
                        &source,
                        &dest,
                        PAYLOAD.as_ptr().cast(),
                        32,
                        opts,
                        reply.as_mut_ptr().cast(),
                        size,
                        TIMEOUT_MS,
                    )
                }
            }
        };
        sent != 0 || event.is_some() && windows::Win32::Foundation::GetLastError() == ERROR_IO_PENDING
    }

    /// The reply in `buf`: who answered, whether it was the destination itself (rather than a
    /// router on the way reporting the hop limit ran out), and Windows' round trip in ms. The
    /// buffer starts zeroed, so a reply without an address is no reply.
    fn parse(ip: IpAddr, buf: &mut [u64]) -> Option<(IpAddr, bool, u32)> {
        let size = (buf.len() * 8) as u32;
        // SAFETY: the buffer holds a reply in the layout the parse functions document.
        unsafe {
            match ip {
                IpAddr::V4(_) => {
                    IcmpParseReplies(buf.as_mut_ptr().cast(), size);
                    let r = &*(buf.as_ptr() as *const ICMP_ECHO_REPLY);
                    let from = IpAddr::V4(Ipv4Addr::from(r.Address.to_ne_bytes()));
                    match r.Status {
                        _ if r.Address == 0 => None,
                        IP_SUCCESS => Some((from, true, r.RoundTripTime)),
                        IP_TTL_EXPIRED_TRANSIT => Some((from, false, r.RoundTripTime)),
                        _ => None,
                    }
                }
                IpAddr::V6(_) => {
                    Icmp6ParseReplies(buf.as_mut_ptr().cast(), size);
                    let r = &*(buf.as_ptr() as *const ICMPV6_ECHO_REPLY_LH);
                    let from = IpAddr::V6(v6_words(r.Address.sin6_addr));
                    match r.Status {
                        _ if from.is_unspecified() => None,
                        IP_SUCCESS => Some((from, true, r.RoundTripTime)),
                        IP_TTL_EXPIRED_TRANSIT => Some((from, false, r.RoundTripTime)),
                        _ => None,
                    }
                }
            }
        }
    }
}

/// Windows' status code and the answering address in a reply buffer, for the log.
fn raw_status(ip: IpAddr, buf: &[u64]) -> (u32, IpAddr) {
    // SAFETY: the buffer is at least as large as either reply structure.
    unsafe {
        match ip {
            IpAddr::V4(_) => {
                let r = &*(buf.as_ptr() as *const ICMP_ECHO_REPLY);
                (r.Status, IpAddr::V4(Ipv4Addr::from(r.Address.to_ne_bytes())))
            }
            IpAddr::V6(_) => {
                let r = &*(buf.as_ptr() as *const ICMPV6_ECHO_REPLY_LH);
                (r.Status, IpAddr::V6(v6_words(r.Address.sin6_addr)))
            }
        }
    }
}

struct Pending {
    target: u32,
    ip: IpAddr,
    t: f64,
    sent: u64,
    event: Owned,
    reply: Box<[u64; REPLY_BYTES / 8]>,
}

/// Pings every target four times a second, each with a one-second timeout, without waiting
/// for one answer before sending the next.
pub fn run_pinger(shared: &Shared) {
    let icmp = Icmp::open();
    log!("pings: IPv4 ready: {}, IPv6 ready: {}", icmp.v4.is_some(), icmp.v6.is_some());
    // Each target's first answer and first failure go in the log.
    let mut answered: HashSet<u32> = HashSet::new();
    let mut failed: HashSet<u32> = HashSet::new();
    let freq = qpc_freq() as f64;
    let mut pending: Vec<Pending> = Vec::new();
    let mut due: HashMap<u32, f64> = HashMap::new();
    while !shared.stopping() {
        let targets = shared.with(|s| s.targets.active()).unwrap_or_default();
        due.retain(|id, _| targets.iter().any(|t| t.0 == *id));
        let now = shared.now();
        for &(target, ip) in &targets {
            let next = due.entry(target).or_insert(now);
            if *next > now || pending.len() >= 60 {
                continue;
            }
            *next = if now - *next > 1_000.0 { now + EVERY_MS } else { *next + EVERY_MS };
            // SAFETY: a manual-reset event, closed by `Owned`.
            let Ok(event) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else { continue };
            let mut p =
                Pending { target, ip, t: now, sent: qpc(), event: Owned(event), reply: Box::new([0; REPLY_BYTES / 8]) };
            // SAFETY: the reply buffer is boxed and kept in `pending` until its event is set.
            if unsafe { icmp.send(ip, Some(p.event.0), None, &mut p.reply[..]) } {
                Counts::add(&shared.counts.pings_sent, 1);
                pending.push(p);
            } else {
                if failed.insert(target) {
                    // SAFETY: reads this thread's last error, set by the failed send.
                    let error = unsafe { windows::Win32::Foundation::GetLastError() };
                    log!("pings: couldn't send to {ip} (target {target}): error {}", error.0);
                }
                shared.with(|s| s.collector.on_ping(target, now, None));
            }
        }
        let until_next = due.values().fold(EVERY_MS, |w, &n| w.min(n - shared.now())).max(1.0);
        if pending.is_empty() {
            std::thread::sleep(Duration::from_secs_f64(until_next / 1000.0));
            continue;
        }
        let events: Vec<HANDLE> = pending.iter().map(|p| p.event.0).collect();
        // SAFETY: every handle in the list is a live event.
        let woke = unsafe { WaitForMultipleObjects(&events, false, until_next as u32) };
        let i = woke.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if i < pending.len() {
            let mut p = pending.swap_remove(i);
            let measured = (qpc() - p.sent) as f64 * 1000.0 / freq;
            let rtt = Icmp::parse(p.ip, &mut p.reply[..])
                .filter(|(from, at_target, _)| *at_target && *from == p.ip)
                .map(|(_, _, ms)| refine_rtt(ms, measured));
            match rtt {
                Some(ms) => {
                    Counts::add(&shared.counts.ping_replies, 1);
                    if answered.insert(p.target) {
                        log!("pings: first answer from {} in {ms:.1} ms", p.ip);
                    }
                }
                None if failed.insert(p.target) => {
                    let (status, from) = raw_status(p.ip, &p.reply[..]);
                    log!("pings: no answer from {} after {measured:.0} ms (status {status}, reply from {from})", p.ip);
                }
                None => {}
            }
            shared.with(|s| s.collector.on_ping(p.target, p.t, rtt));
        }
    }
    // Requests still out write into their buffers when they time out; wait for that.
    for p in pending {
        // SAFETY: live event handle.
        unsafe { WaitForSingleObject(p.event.0, TIMEOUT_MS + 1_000) };
    }
}

impl Drop for Icmp {
    fn drop(&mut self) {
        for h in [self.v4, self.v6].into_iter().flatten() {
            // SAFETY: closes a handle this value opened, with the ICMP close function.
            let _ = unsafe { IcmpCloseHandle(h) };
        }
    }
}

/* ---------- traceroute ---------- */

/// One synchronous probe: with `ttl`, who answered at that many hops; without, whether the
/// address itself answers.
fn probe(icmp: &Icmp, ip: IpAddr, ttl: Option<u8>) -> Option<(IpAddr, bool)> {
    let mut reply = [0u64; REPLY_BYTES / 8];
    // SAFETY: without an event the call returns only once the reply is in the buffer.
    unsafe { icmp.send(ip, None, ttl, &mut reply) };
    Icmp::parse(ip, &mut reply).map(|(from, at_target, _)| (from, at_target && from == ip))
}

/// Finds the hops toward the game server, all hop limits at once, and whether the server
/// answers pings itself.
pub fn route_to(server: IpAddr, shared: &Shared) -> Route {
    const MAX_HOPS: u8 = 24;
    let results: Vec<Option<(IpAddr, bool)>> = std::thread::scope(|scope| {
        let probes: Vec<_> = (1..=MAX_HOPS)
            .map(|ttl| {
                std::thread::Builder::new()
                    .stack_size(128 * 1024)
                    .spawn_scoped(scope, move || probe(&Icmp::open(), server, Some(ttl)))
                    .ok()
            })
            .collect();
        probes.into_iter().map(|h| h.and_then(|h| h.join().ok()).flatten()).collect()
    });
    let mut route = Route::default();
    for r in results {
        route.hops.push(r.map(|(from, _)| from));
        if r.is_some_and(|(_, reached)| reached) {
            route.server_answers = true;
            break;
        }
    }
    if !route.server_answers {
        let icmp = Icmp::open();
        route.server_answers =
            (0..3).any(|_| !shared.stopping() && probe(&icmp, server, None).is_some_and(|(_, reached)| reached));
    }
    route
}
