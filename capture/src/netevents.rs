//! Reading send and receive events from Windows' own network trace (the
//! Microsoft-Windows-Kernel-Network provider), which tells which process sent or received
//! how many bytes to which address, without looking inside any packet.

use crate::collector::Packet;
use culpritcore::net::Proto;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The provider's events that carry a send or a receive, by event id.
pub const SEND_RECV_EVENTS: [u16; 8] = [10, 11, 26, 27, 42, 43, 58, 59];

/// Turns one event into a packet. Every one of these events starts with the same fields:
/// process id, byte count, remote address, local address, remote port, local port. The
/// addresses and ports are in network byte order. Traffic within this PC is left out.
pub fn parse(id: u16, data: &[u8], t: f64) -> Option<Packet> {
    let (proto, v6, inbound) = match id {
        10 => (Proto::Tcp, false, false),
        11 => (Proto::Tcp, false, true),
        26 => (Proto::Tcp, true, false),
        27 => (Proto::Tcp, true, true),
        42 => (Proto::Udp, false, false),
        43 => (Proto::Udp, false, true),
        58 => (Proto::Udp, true, false),
        59 => (Proto::Udp, true, true),
        _ => return None,
    };
    let addr_len = if v6 { 16 } else { 4 };
    let port_at = 8 + 2 * addr_len;
    let field = |at: usize, len: usize| data.get(at..at + len);
    let pid = u32::from_le_bytes(field(0, 4)?.try_into().ok()?);
    let bytes = u32::from_le_bytes(field(4, 4)?.try_into().ok()?);
    let raw = field(8, addr_len)?;
    let remote = if v6 {
        IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(raw).ok()?))
    } else {
        IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(raw).ok()?))
    };
    let port = u16::from_be_bytes(field(port_at, 2)?.try_into().ok()?);
    if remote.is_loopback() || remote.is_unspecified() {
        return None;
    }
    // The collector only trusts the remote port on sends.
    Some(Packet { t, pid, remote, port: (!inbound).then_some(port), proto, inbound, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(pid: u32, size: u32, remote: &[u8], local: &[u8], rport: u16, lport: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&pid.to_le_bytes());
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(remote);
        v.extend_from_slice(local);
        v.extend_from_slice(&rport.to_be_bytes());
        v.extend_from_slice(&lport.to_be_bytes());
        // startime, endtime, seqnum, connid follow on some events.
        v.extend_from_slice(&[0u8; 20]);
        v
    }

    #[test]
    fn reads_udp_and_tcp_both_ways() {
        let data = payload(4120, 80, &[155, 133, 250, 9], &[192, 168, 1, 20], 27015, 50000);
        let p = parse(42, &data, 12.5).unwrap();
        assert_eq!(p.pid, 4120);
        assert_eq!(p.remote, "155.133.250.9".parse::<IpAddr>().unwrap());
        assert_eq!(p.port, Some(27015));
        assert_eq!((p.proto, p.inbound, p.bytes, p.t), (Proto::Udp, false, 80, 12.5));

        let p = parse(11, &data, 0.0).unwrap();
        assert_eq!((p.proto, p.inbound, p.port), (Proto::Tcp, true, None));

        let v6: Ipv6Addr = "2606:4700::1111".parse().unwrap();
        let data = payload(7, 1400, &v6.octets(), &Ipv6Addr::LOCALHOST.octets(), 443, 1);
        let p = parse(26, &data, 0.0).unwrap();
        assert_eq!((p.remote, p.port, p.bytes), (IpAddr::V6(v6), Some(443), 1400));
    }

    #[test]
    fn skips_other_events_local_traffic_and_short_payloads() {
        let data = payload(1, 1, &[8, 8, 8, 8], &[10, 0, 0, 2], 53, 1);
        assert!(parse(12, &data, 0.0).is_none());
        assert!(parse(43, &data[..17], 0.0).is_none());
        assert!(parse(43, &payload(1, 1, &[127, 0, 0, 1], &[127, 0, 0, 1], 1, 2), 0.0).is_none());
        assert!(parse(43, &data, 0.0).is_some());
    }
}
