//! Windows' own network trace (Event Tracing for Windows, the Microsoft-Windows-Kernel-Network
//! provider). For every send and receive it says which process, how many bytes and which
//! address, stamped with the performance counter. Nothing inside a packet is read.

use super::util::{join_within, wide};
use super::Shared;
use crate::collector::Packet;
use crate::netevents::{self, SEND_RECV_EVENTS};
use std::cell::RefCell;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_SUCCESS};
use windows::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE,
    ENABLE_TRACE_PARAMETERS, ENABLE_TRACE_PARAMETERS_VERSION_2, EVENT_CONTROL_CODE_ENABLE_PROVIDER,
    EVENT_FILTER_DESCRIPTOR, EVENT_FILTER_TYPE_EVENT_ID, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
    EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD,
    PROCESS_TRACE_MODE_RAW_TIMESTAMP, PROCESS_TRACE_MODE_REAL_TIME, TRACE_LEVEL_INFORMATION, WNODE_FLAG_TRACED_GUID,
};

const KERNEL_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
/// IPv4 and IPv6 events.
const KEYWORDS: u64 = 0x10 | 0x20;
const SESSION: &str = "CulpritNetwork";
/// Packets handed to the collector at once.
const BATCH: usize = 256;

/// EVENT_TRACE_PROPERTIES followed by room for the session name, as the calls want it.
struct Properties(Vec<u64>);

impl Properties {
    fn new() -> Self {
        let head = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
        let words = (head + 1024) / 8;
        let mut buf = Properties(vec![0u64; words]);
        let p = buf.get();
        p.Wnode.BufferSize = (words * 8) as u32;
        p.LoggerNameOffset = head as u32;
        buf
    }

    fn get(&mut self) -> &mut EVENT_TRACE_PROPERTIES {
        // SAFETY: the buffer is u64-aligned and larger than the structure.
        unsafe { &mut *(self.0.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES) }
    }
}

/// Stops a trace session by name; used for this trace, and for PresentMon's session, which
/// makes PresentMon finish and exit on its own.
pub fn stop_session(name: &str) {
    let name = wide(name);
    let mut props = Properties::new();
    // SAFETY: the properties buffer has room for the session name the call writes back.
    let _ = unsafe {
        ControlTraceW(CONTROLTRACE_HANDLE::default(), PCWSTR(name.as_ptr()), props.get(), EVENT_TRACE_CONTROL_STOP)
    };
}

fn start_session() -> Result<(), String> {
    let name = wide(SESSION);
    let start = || {
        let mut props = Properties::new();
        let p = props.get();
        p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        p.Wnode.ClientContext = 1; // stamp events with the performance counter
        p.BufferSize = 64; // KB
        p.MinimumBuffers = 4;
        p.MaximumBuffers = 32;
        p.FlushTimer = 1; // deliver at least once a second
        p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        let mut handle = CONTROLTRACE_HANDLE::default();
        // SAFETY: the name and properties outlive the call.
        let status = unsafe { StartTraceW(&mut handle, PCWSTR(name.as_ptr()), props.get()) };
        (status, handle)
    };
    let (mut status, mut handle) = start();
    if status == ERROR_ALREADY_EXISTS {
        // Left over from a recording that didn't stop cleanly.
        stop_session(SESSION);
        (status, handle) = start();
    }
    if status != ERROR_SUCCESS {
        return Err(format!("couldn't start the trace session (error {})", status.0));
    }
    // Only sends and receives: the provider also reports connects, retransmits and copies.
    let mut filter = vec![0u16; 2 + SEND_RECV_EVENTS.len()];
    filter[0] = 1; // FilterIn = true, Reserved = 0
    filter[1] = SEND_RECV_EVENTS.len() as u16;
    filter[2..].copy_from_slice(&SEND_RECV_EVENTS);
    let mut desc = EVENT_FILTER_DESCRIPTOR {
        Ptr: filter.as_ptr() as u64,
        Size: (filter.len() * 2) as u32,
        Type: EVENT_FILTER_TYPE_EVENT_ID,
    };
    let params = ENABLE_TRACE_PARAMETERS {
        Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
        EnableFilterDesc: &mut desc,
        FilterDescCount: 1,
        ..Default::default()
    };
    let enable = |params: Option<*const ENABLE_TRACE_PARAMETERS>| {
        // SAFETY: the filter and parameters outlive the call.
        unsafe {
            EnableTraceEx2(
                handle,
                &KERNEL_NETWORK,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                TRACE_LEVEL_INFORMATION as u8,
                KEYWORDS,
                0,
                0,
                params,
            )
        }
    };
    // Older Windows may not filter by event id; then take everything and skip the rest.
    let mut status = enable(Some(&params));
    if status != ERROR_SUCCESS {
        status = enable(None);
    }
    if status != ERROR_SUCCESS {
        stop_session(SESSION);
        return Err(format!("couldn't enable the network provider (error {})", status.0));
    }
    Ok(())
}

/// What the event callback writes into. Only the trace's own thread touches it.
struct Sink {
    shared: Arc<Shared>,
    batch: RefCell<Vec<Packet>>,
}

impl Sink {
    fn flush(&self) {
        let mut batch = self.batch.borrow_mut();
        if !batch.is_empty() {
            self.shared.with(|s| {
                for p in batch.iter() {
                    s.collector.on_packet(*p);
                }
            });
            batch.clear();
        }
    }
}

unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    // SAFETY: ETW passes a valid record whose UserContext is the Sink set in OpenTraceW,
    // alive until ProcessTrace returns; UserData holds UserDataLength bytes.
    let (r, sink) = unsafe { (&*record, &*((*record).UserContext as *const Sink)) };
    let h = &r.EventHeader;
    if h.ProviderId != KERNEL_NETWORK || r.UserData.is_null() {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(r.UserData as *const u8, r.UserDataLength as usize) };
    let t = sink.shared.clock.ms(h.TimeStamp as u64);
    if let Some(p) = netevents::parse(h.EventDescriptor.Id, data, t) {
        let full = {
            let mut batch = sink.batch.borrow_mut();
            batch.push(p);
            batch.len() >= BATCH
        };
        if full {
            sink.flush();
        }
    }
}

/// Called after each buffer of events, at least once a second: hand over what's waiting.
unsafe extern "system" fn on_buffer(logfile: *mut EVENT_TRACE_LOGFILEW) -> u32 {
    // SAFETY: Context is the Sink set in OpenTraceW.
    let sink = unsafe { &*((*logfile).Context as *const Sink) };
    sink.flush();
    1
}

pub struct NetTrace {
    thread: JoinHandle<()>,
}

impl NetTrace {
    pub fn start(shared: Arc<Shared>) -> Result<NetTrace, String> {
        start_session()?;
        let (ready, opened) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("culprit network trace".into())
            .spawn(move || {
                let sink = Box::new(Sink { shared, batch: RefCell::new(Vec::with_capacity(BATCH)) });
                let mut name = wide(SESSION);
                let mut logfile = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(name.as_mut_ptr()), ..Default::default() };
                logfile.Anonymous1.ProcessTraceMode =
                    PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
                logfile.Anonymous2.EventRecordCallback = Some(on_event);
                logfile.BufferCallback = Some(on_buffer);
                logfile.Context = &*sink as *const Sink as *mut _;
                // SAFETY: the name and sink outlive ProcessTrace, which runs on this thread
                // until the session stops.
                let trace = unsafe { OpenTraceW(&mut logfile) };
                if trace.Value == u64::MAX {
                    let _ = ready.send(Err("couldn't open the trace session".to_string()));
                    return;
                }
                let _ = ready.send(Ok(()));
                unsafe {
                    let _ = ProcessTrace(&[trace], None, None);
                    let _ = CloseTrace(PROCESSTRACE_HANDLE { Value: trace.Value });
                }
                sink.flush();
            })
            .map_err(|e| e.to_string())?;
        match opened.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(NetTrace { thread }),
            Ok(Err(e)) => {
                stop_session(SESSION);
                Err(e)
            }
            Err(_) => {
                stop_session(SESSION);
                Err("the trace session didn't open".into())
            }
        }
    }

    pub fn stop(self) {
        stop_session(SESSION);
        join_within(self.thread, Duration::from_secs(5));
    }
}
