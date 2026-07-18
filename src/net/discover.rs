//! UDP LAN discovery: broadcast + directed subnet + multicast + unicast to known peers.
//!
//! Why not only `255.255.255.255`?
//! - On Windows multi-NIC / VPN / some VM bridges, limited broadcast often leaves via
//!   the wrong interface or is dropped. Directed broadcasts and multicast are more reliable.
//! - Once a peer was ever seen (clients.json), we also unicast announces so discovery
//!   keeps working even when L2 broadcast is broken.

use crate::protocol::{
    DiscoverPacket, CAP_FILE, CAP_IMAGE, CAP_TEXT, DISCOVER_ANNOUNCE, DISCOVER_GOODBYE,
    PROTOCOL_VERSION,
};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

/// Site-local multicast for OhMyCopy discovery (not routed by default).
const MULTICAST_V4: Ipv4Addr = Ipv4Addr::new(239, 255, 90, 71);

/// How long a peer stays in the nearby list without a fresh packet.
const STALE_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub device_id: Uuid,
    pub name: String,
    pub addr: SocketAddr,
    pub last_seen: Instant,
    pub caps: u32,
}

pub struct DiscoveryService {
    local_id: Uuid,
    local_name: String,
    tcp_port: u16,
    udp_port: u16,
    interval: Duration,
    devices: Arc<RwLock<Vec<DiscoveredDevice>>>,
    /// Extra unicast targets (typically from clients.json) — refreshed by the app.
    known_targets: Arc<RwLock<Vec<SocketAddr>>>,
}

impl DiscoveryService {
    pub fn new(
        local_id: Uuid,
        local_name: String,
        tcp_port: u16,
        udp_port: u16,
        interval_secs: u64,
    ) -> Self {
        Self {
            local_id,
            local_name,
            tcp_port,
            udp_port,
            interval: Duration::from_secs(interval_secs.max(2)),
            devices: Arc::new(RwLock::new(Vec::new())),
            known_targets: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn devices_handle(&self) -> Arc<RwLock<Vec<DiscoveredDevice>>> {
        self.devices.clone()
    }

    pub fn known_targets_handle(&self) -> Arc<RwLock<Vec<SocketAddr>>> {
        self.known_targets.clone()
    }

    pub async fn run(self, mut shutdown: mpsc::Receiver<()>) {
        let sock = match bind_discovery_socket(self.udp_port).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, port = self.udp_port, "UDP bind failed (discovery disabled)");
                let _ = shutdown.recv().await;
                return;
            }
        };

        join_multicast_all(&sock);

        let sock = Arc::new(sock);
        let devices = self.devices.clone();
        let local_id = self.local_id;
        let udp_port = self.udp_port;

        tracing::info!(
            port = udp_port,
            multicast = %MULTICAST_V4,
            "UDP discovery listening"
        );

        // receiver
        let recv_sock = sock.clone();
        let recv_devices = devices.clone();
        let recv_task = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                match recv_sock.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        if let Some(pkt) = DiscoverPacket::decode(&buf[..n]) {
                            if pkt.device_id == local_id {
                                continue;
                            }
                            if pkt.msg_type == DISCOVER_GOODBYE {
                                let mut list = recv_devices.write().await;
                                list.retain(|d| d.device_id != pkt.device_id);
                                tracing::debug!(peer = %pkt.device_id, "discovery goodbye");
                                continue;
                            }
                            if pkt.msg_type != DISCOVER_ANNOUNCE && pkt.msg_type != 0 {
                                // Accept unknown types that look like announce if they have name.
                            }
                            let tcp = if pkt.tcp_port == 0 {
                                // Fall back: same host, default or UDP peer port.
                                from.port()
                            } else {
                                pkt.tcp_port
                            };
                            let addr = SocketAddr::new(from.ip(), tcp);
                            let mut list = recv_devices.write().await;
                            if let Some(existing) =
                                list.iter_mut().find(|d| d.device_id == pkt.device_id)
                            {
                                let name_changed = existing.name != pkt.name;
                                let addr_changed = existing.addr != addr;
                                existing.name = pkt.name.clone();
                                existing.addr = addr;
                                existing.last_seen = Instant::now();
                                existing.caps = pkt.caps;
                                if name_changed || addr_changed {
                                    tracing::info!(
                                        name = %existing.name,
                                        %addr,
                                        id = %pkt.device_id,
                                        "discovery updated peer"
                                    );
                                }
                            } else {
                                tracing::info!(
                                    name = %pkt.name,
                                    %addr,
                                    id = %pkt.device_id,
                                    from = %from,
                                    "discovery found peer"
                                );
                                list.push(DiscoveredDevice {
                                    device_id: pkt.device_id,
                                    name: pkt.name,
                                    addr,
                                    last_seen: Instant::now(),
                                    caps: pkt.caps,
                                });
                            }
                        } else {
                            tracing::trace!(from = %from, bytes = n, "UDP packet ignored (not OMCP discover)");
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "udp recv");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        });

        // announce loop — burst for the first few seconds so peers appear quickly
        let mut tick_n: u64 = 0;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    let goodbye = DiscoverPacket {
                        version: PROTOCOL_VERSION,
                        msg_type: DISCOVER_GOODBYE,
                        device_id: self.local_id,
                        tcp_port: self.tcp_port,
                        name: self.local_name.clone(),
                        caps: CAP_TEXT | CAP_IMAGE | CAP_FILE,
                    };
                    let payload = goodbye.encode();
                    let dests = self.collect_destinations().await;
                    for dest in dests {
                        let _ = sock.send_to(&payload, dest).await;
                    }
                    recv_task.abort();
                    break;
                }
                _ = ticker.tick() => {
                    tick_n = tick_n.saturating_add(1);
                    // Expire stale
                    {
                        let mut list = devices.write().await;
                        list.retain(|d| d.last_seen.elapsed() < Duration::from_secs(STALE_SECS));
                    }

                    // Burst every 1s for ~12s, then back to configured interval.
                    let should_announce = tick_n <= 12
                        || tick_n.is_multiple_of(self.interval.as_secs().max(1));
                    if !should_announce {
                        continue;
                    }

                    let pkt = DiscoverPacket {
                        version: PROTOCOL_VERSION,
                        msg_type: DISCOVER_ANNOUNCE,
                        device_id: self.local_id,
                        tcp_port: self.tcp_port,
                        name: self.local_name.clone(),
                        caps: CAP_TEXT | CAP_IMAGE | CAP_FILE,
                    };
                    let payload = pkt.encode();
                    let dests = self.collect_destinations().await;
                    let mut ok = 0u32;
                    let mut err = 0u32;
                    for dest in &dests {
                        match sock.send_to(&payload, *dest).await {
                            Ok(_) => ok += 1,
                            Err(e) => {
                                err += 1;
                                tracing::debug!(error = %e, %dest, "udp announce send failed");
                            }
                        }
                    }
                    if tick_n <= 3 || tick_n.is_multiple_of(30) {
                        tracing::info!(
                            targets = dests.len(),
                            ok,
                            err,
                            "UDP discovery announce"
                        );
                    }
                }
            }
        }
    }

    async fn collect_destinations(&self) -> Vec<SocketAddr> {
        let mut dests = lan_broadcast_targets(self.udp_port);
        // Multicast group
        dests.push(SocketAddr::new(std::net::IpAddr::V4(MULTICAST_V4), self.udp_port));
        // Known peers (clients.json etc.) — unicast announce so discovery survives broken broadcast
        {
            let known = self.known_targets.read().await;
            for a in known.iter() {
                // Announce to their UDP port (same as configured UDP; fall back to peer port).
                let udp_addr = SocketAddr::new(a.ip(), self.udp_port);
                dests.push(udp_addr);
                if a.port() != self.udp_port {
                    dests.push(*a);
                }
            }
        }
        dests.sort_by_key(|a| (a.ip().to_string(), a.port()));
        dests.dedup();
        dests
    }
}

/// Build IPv4 UDP socket with broadcast + reuse, converted to Tokio.
async fn bind_discovery_socket(port: u16) -> std::io::Result<UdpSocket> {
    let domain = socket2::Domain::IPV4;
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
    {
        let _ = socket.set_reuse_port(true);
    }
    socket.set_broadcast(true)?;
    // Allow receiving our own multicasts on some stacks; we filter by device_id.
    let _ = socket.set_multicast_loop_v4(true);
    let _ = socket.set_multicast_ttl_v4(1);

    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    socket.bind(&addr.into())?;
    socket.set_nonblocking(true)?;

    let std_sock: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_sock)
}

fn join_multicast_all(sock: &UdpSocket) {
    // Default: all interfaces
    if let Err(e) = sock.join_multicast_v4(MULTICAST_V4, Ipv4Addr::UNSPECIFIED) {
        tracing::debug!(error = %e, "join_multicast_v4 UNSPECIFIED failed");
    }
    // Per-interface join (Windows often needs this)
    for ip in local_ipv4_addrs() {
        if let Err(e) = sock.join_multicast_v4(MULTICAST_V4, ip) {
            tracing::debug!(error = %e, %ip, "join_multicast_v4 iface failed");
        } else {
            tracing::debug!(%ip, multicast = %MULTICAST_V4, "joined discovery multicast");
        }
    }
}

fn local_ipv4_addrs() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return out;
    };
    for iface in ifaces {
        if iface.is_loopback() {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = iface.addr {
            if !v4.ip.is_link_local() {
                out.push(v4.ip);
            }
        }
    }
    out
}

/// Limited broadcast + directed subnet broadcasts for every IPv4 interface.
fn lan_broadcast_targets(udp_port: u16) -> Vec<SocketAddr> {
    let mut dests = vec![SocketAddr::from((Ipv4Addr::BROADCAST, udp_port))];
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return dests;
    };
    for iface in ifaces {
        if iface.is_loopback() {
            continue;
        }
        let if_addrs::IfAddr::V4(v4) = iface.addr else {
            continue;
        };
        if v4.ip.is_link_local() {
            continue;
        }
        let ip = u32::from(v4.ip);
        let mask = u32::from(v4.netmask);
        if mask == 0 {
            continue;
        }
        let bcast = if let Some(b) = v4.broadcast {
            b
        } else {
            Ipv4Addr::from(ip | !mask)
        };
        // Skip nonsense
        if bcast.is_unspecified() || bcast.is_loopback() {
            continue;
        }
        dests.push(SocketAddr::from((bcast, udp_port)));
    }
    dests.sort_by_key(|a| a.ip().to_string());
    dests.dedup();
    dests
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_targets_include_limited() {
        let t = lan_broadcast_targets(3721);
        assert!(t.iter().any(|a| a.ip() == Ipv4Addr::BROADCAST));
    }
}
