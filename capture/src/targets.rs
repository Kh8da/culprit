//! Which addresses to ping: the router from the start, then the provider's first hop and
//! the game server (or the last stop before it) once the game is talking to one.

use crate::collector::Collector;
use culpritcore::model::TargetRole;
use culpritcore::net::{isp_hop, last_answering_hop};
use std::net::IpAddr;

/// What a traceroute toward the game server found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Route {
    /// Hop 1 first, `None` for hops that didn't answer. Ends at the server if it answered.
    pub hops: Vec<Option<IpAddr>>,
    /// Whether the server itself answers pings.
    pub server_answers: bool,
}

/// The targets being pinged right now, kept in step with the collector's.
#[derive(Default)]
pub struct Targets {
    router: Option<(u32, IpAddr)>,
    isp: Option<(u32, IpAddr)>,
    server: Option<(u32, TargetRole, IpAddr)>,
}

impl Targets {
    /// The router changed (or was found); `None` when there is none to ping.
    pub fn set_router(&mut self, c: &mut Collector, ip: Option<IpAddr>, t: f64) {
        if self.router.map(|r| r.1) == ip {
            return;
        }
        if let Some((id, _)) = self.router.take() {
            c.end_target(id, t);
        }
        self.router = ip.map(|ip| (c.add_target(TargetRole::Router, ip, None, t), ip));
    }

    /// The game moved to `server`, and this is the way to it.
    pub fn set_route(&mut self, c: &mut Collector, server: IpAddr, route: &Route, t: f64) {
        // The provider's first hop is the same whichever server; keep the last one found
        // if this traceroute missed it.
        if let Some(isp) = isp_hop(&route.hops).filter(|ip| *ip != server) {
            if self.isp.map(|x| x.1) != Some(isp) {
                if let Some((id, _)) = self.isp.take() {
                    c.end_target(id, t);
                }
                self.isp = Some((c.add_target(TargetRole::Isp, isp, None, t), isp));
            }
        }
        // A server that ignores pings is measured at the last hop that answers, unless that
        // is the provider's hop, which would blame the provider for the server's trouble.
        let path = last_answering_hop(&route.hops, &server).filter(|hop| Some(*hop) != self.isp.map(|x| x.1));
        let (role, ip) = match path {
            Some(hop) if !route.server_answers => (TargetRole::ServerPath, hop),
            _ => (TargetRole::Server, server),
        };
        if self.server.is_some_and(|(_, r, i)| r == role && i == ip) {
            return;
        }
        if let Some((id, ..)) = self.server.take() {
            c.end_target(id, t);
        }
        self.server = Some((c.add_target(role, ip, None, t), role, ip));
    }

    /// Every target to ping, by the collector's target id.
    pub fn active(&self) -> Vec<(u32, IpAddr)> {
        self.router.iter().chain(self.isp.iter()).copied().chain(self.server.map(|(id, _, ip)| (id, ip))).collect()
    }
}

/// A round trip with better than whole-millisecond resolution. Windows measures it in
/// whole milliseconds; the recorder's own measurement is finer but includes the time its
/// thread took to wake up, so it is only trusted inside Windows' millisecond.
pub fn refine_rtt(windows_ms: u32, measured_ms: f64) -> f32 {
    let w = windows_ms as f64;
    measured_ms.clamp(w, w + 0.999) as f32
}

/// Wi-Fi signal quality, 0 to 100, from its strength in dBm: Windows maps -100 dBm to 0
/// and -50 dBm to 100, in a straight line.
pub fn wifi_quality(rssi_dbm: i32) -> f32 {
    ((rssi_dbm as f32 + 100.0) * 2.0).clamp(0.0, 100.0)
}

/// The Wi-Fi band for a channel number. 6 GHz channels reuse the same numbers, so a 6 GHz
/// connection reads as 2.4 or 5 here.
pub fn wifi_band(channel: u32) -> Option<f32> {
    match channel {
        1..=14 => Some(2.4),
        32..=177 => Some(5.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::Options;
    use culpritcore::model::{Connection, Machine};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn collector() -> Collector {
        Collector::new(Options::default(), 0, Machine::default(), Connection::default())
    }

    #[test]
    fn router_then_route() {
        let mut c = collector();
        let mut t = Targets::default();
        t.set_router(&mut c, Some(ip("192.168.1.1")), 0.0);
        t.set_router(&mut c, Some(ip("192.168.1.1")), 500.0);
        assert_eq!(t.active(), vec![(1, ip("192.168.1.1"))]);

        let server = ip("155.133.250.9");
        let hops = vec![Some(ip("192.168.1.1")), None, Some(ip("84.116.0.1")), Some(ip("80.1.2.3")), Some(server)];
        t.set_route(&mut c, server, &Route { hops: hops.clone(), server_answers: true }, 3_000.0);
        assert_eq!(t.active(), vec![(1, ip("192.168.1.1")), (2, ip("84.116.0.1")), (3, server)]);

        // The game moves to a server that ignores pings: ping the last hop before it instead.
        let quiet = ip("162.254.1.1");
        let mut hops2 = hops[..4].to_vec();
        hops2.push(None);
        t.set_route(&mut c, quiet, &Route { hops: hops2, server_answers: false }, 9_000.0);
        assert_eq!(t.active(), vec![(1, ip("192.168.1.1")), (2, ip("84.116.0.1")), (4, ip("80.1.2.3"))]);

        // On Wi-Fi now, with a new router.
        t.set_router(&mut c, Some(ip("10.0.0.1")), 12_000.0);
        let rec = c.finish(15_000.0);
        let roles: Vec<(TargetRole, Option<f64>)> = rec.targets.iter().map(|x| (x.role, x.until)).collect();
        assert_eq!(
            roles,
            vec![
                (TargetRole::Router, Some(12_000.0)),
                (TargetRole::Isp, Some(15_000.0)),
                (TargetRole::Server, Some(9_000.0)),
                (TargetRole::ServerPath, Some(15_000.0)),
                (TargetRole::Router, Some(15_000.0)),
            ]
        );
    }

    #[test]
    fn unpingable_server_with_no_path_is_still_tried() {
        let mut c = collector();
        let mut t = Targets::default();
        let server = ip("155.133.250.9");
        // Only the provider's hop answered, so it can't stand in for the server.
        let hops = vec![Some(ip("192.168.1.1")), Some(ip("84.116.0.1")), None, None];
        t.set_route(&mut c, server, &Route { hops, server_answers: false }, 0.0);
        assert_eq!(t.active(), vec![(1, ip("84.116.0.1")), (2, server)]);
        assert_eq!(c.finish(1.0).targets[1].role, TargetRole::Server);
    }

    #[test]
    fn conversions() {
        assert_eq!(refine_rtt(0, 0.42), 0.42);
        assert_eq!(refine_rtt(12, 19.0), 12.999);
        assert_eq!(refine_rtt(12, 3.0), 12.0);
        assert_eq!(wifi_quality(-50), 100.0);
        assert_eq!(wifi_quality(-75), 50.0);
        assert_eq!(wifi_quality(-120), 0.0);
        assert_eq!(wifi_band(6), Some(2.4));
        assert_eq!(wifi_band(149), Some(5.0));
        assert_eq!(wifi_band(0), None);
    }
}
