//! Telling home, provider and internet addresses apart, and spotting which of a game's
//! connections is the game server.

use std::net::{IpAddr, Ipv4Addr};

/// Addresses that never leave your home or your own PC.
pub fn is_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // unique local
                || (seg0 & 0xffc0) == 0xfe80 // link local
        }
    }
}

/// Carrier-grade NAT space: shared by an internet provider's customers, so it is the provider's.
pub fn is_cgnat(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (64..128).contains(&o[1])
        }
        IpAddr::V6(_) => false,
    }
}

pub fn is_multicast(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_multicast() || *v4 == Ipv4Addr::BROADCAST,
        IpAddr::V6(v6) => v6.is_multicast(),
    }
}

/// Given the hops a traceroute found (hop 1 first, `None` for hops that didn't answer),
/// the first one past your home network is your internet provider's.
pub fn isp_hop(hops: &[Option<IpAddr>]) -> Option<IpAddr> {
    hops.iter().skip(1).flatten().find(|ip| !is_local(ip)).copied()
}

/// The last hop that answered before the destination, for servers that ignore pings.
pub fn last_answering_hop(hops: &[Option<IpAddr>], destination: &IpAddr) -> Option<IpAddr> {
    hops.iter().rev().flatten().find(|ip| *ip != destination && !is_local(ip)).copied()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Proto {
    Udp,
    Tcp,
}

/// What a game exchanged with one remote address and port over a few seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flow {
    pub ip: IpAddr,
    pub port: u16,
    pub proto: Proto,
    pub packets_in: u32,
    pub packets_out: u32,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

/// Picks the game server out of a game's flows seen over `seconds`.
///
/// Game servers trade a steady stream of small packets both ways. UDP flows win, since
/// almost every action game uses UDP; a TCP flow only counts when it looks the same
/// (small packets, many per second), which rules out downloads, store pages and telemetry.
pub fn pick_server(flows: &[Flow], seconds: f64, allow_local: bool) -> Option<Flow> {
    let seconds = seconds.max(0.001);
    let eligible = |f: &&Flow| {
        !is_multicast(&f.ip)
            && (allow_local || !is_local(&f.ip))
            && f.port != 53
            && f.port != 5353
            && f.port != 1900
            && f.packets_in as f64 / seconds >= 5.0
            && f.packets_out as f64 / seconds >= 2.0
    };
    let rate = |f: &Flow| (f.packets_in + f.packets_out) as f64;
    let best_udp =
        flows.iter().filter(eligible).filter(|f| f.proto == Proto::Udp).max_by(|a, b| rate(a).total_cmp(&rate(b)));
    if let Some(f) = best_udp {
        return Some(*f);
    }
    flows
        .iter()
        .filter(eligible)
        .filter(|f| f.proto == Proto::Tcp)
        .filter(|f| {
            let packets = (f.packets_in + f.packets_out).max(1) as u64;
            (f.bytes_in + f.bytes_out) / packets < 600
        })
        .max_by(|a, b| rate(a).total_cmp(&rate(b)))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classifies_addresses() {
        assert!(is_local(&ip("192.168.1.1")));
        assert!(is_local(&ip("10.0.0.138")));
        assert!(is_local(&ip("fe80::1")));
        assert!(is_local(&ip("fd00::1")));
        assert!(!is_local(&ip("8.8.8.8")));
        assert!(!is_local(&ip("100.70.0.1")));
        assert!(is_cgnat(&ip("100.70.0.1")));
        assert!(!is_cgnat(&ip("100.128.0.1")));
    }

    #[test]
    fn finds_isp_hop_past_double_nat() {
        let hops =
            vec![Some(ip("192.168.1.1")), Some(ip("192.168.0.1")), None, Some(ip("100.64.12.1")), Some(ip("80.1.2.3"))];
        assert_eq!(isp_hop(&hops), Some(ip("100.64.12.1")));
        assert_eq!(last_answering_hop(&hops, &ip("155.133.1.1")), Some(ip("80.1.2.3")));
        assert_eq!(isp_hop(&[Some(ip("192.168.1.1"))]), None);
    }

    fn flow(addr: &str, port: u16, proto: Proto, pin: u32, pout: u32, bytes: u64) -> Flow {
        Flow { ip: ip(addr), port, proto, packets_in: pin, packets_out: pout, bytes_in: bytes, bytes_out: bytes }
    }

    #[test]
    fn picks_udp_game_server_over_downloads() {
        let flows = [
            flow("23.1.1.1", 443, Proto::Tcp, 9000, 3000, 12_000_000), // patch download
            flow("155.133.250.9", 27015, Proto::Udp, 320, 320, 40_000),
            flow("8.8.8.8", 53, Proto::Udp, 50, 50, 5_000),
            flow("192.168.1.20", 9000, Proto::Udp, 900, 900, 10_000), // LAN device
        ];
        let picked = pick_server(&flows, 5.0, false).unwrap();
        assert_eq!(picked.ip, ip("155.133.250.9"));
        assert_eq!(pick_server(&flows, 5.0, true).unwrap().ip, ip("192.168.1.20"));
    }

    #[test]
    fn tcp_game_needs_small_steady_packets() {
        let flows = [
            flow("23.1.1.1", 443, Proto::Tcp, 9000, 3000, 12_000_000),
            flow("37.244.1.1", 3724, Proto::Tcp, 100, 60, 8_000),
        ];
        assert_eq!(pick_server(&flows, 5.0, false).unwrap().ip, ip("37.244.1.1"));
        assert_eq!(pick_server(&flows[..1], 5.0, false), None);
        assert_eq!(pick_server(&[flow("155.133.250.9", 27015, Proto::Udp, 3, 3, 300)], 5.0, false), None);
    }
}
