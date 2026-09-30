// Captures and relays Rocket League's LAN discovery beacon. Game traffic
// never goes through Hebnix: once RL is bound to its tailnet address via
// -multihome, its own UDP sockets talk directly to the peer over
// tailscaled's WireGuard tunnel. The only thing Hebnix still does is make
// sure a guest's RL process learns the host's *tailnet* address in the
// first place, since RL's own beacon broadcasts the host's real LAN ip:port.
//
// RL sends a global UDP broadcast (255.255.255.255, one of
// RL_DISCOVERY_PORTS) from its tailnet-bound socket. A second socket sharing
// that port doesn't reliably get a copy (the Windows port found the same),
// so, like the Windows port's WinDivert sniff handle, this watches below the
// socket layer: an AF_PACKET SOCK_DGRAM socket (needs CAP_NET_RAW) sees every
// packet leaving any interface, and only outgoing IPv4 UDP broadcasts to a
// discovery port are kept. It only copies packets, so RL's own broadcast is
// undisturbed, and the relay's own sends are unicast, so it never recaptures
// them. Sending the rewritten copy out uses an ordinary socket.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use socket2::{Domain, Socket, Type};

// linux/if_ether.h, linux/if_packet.h
const ETH_P_IP: u16 = 0x0800;
const PACKET_OUTGOING: u8 = 4;

pub struct BeaconRelay {
    // bound to an OS-assigned ephemeral port on the tailnet address, used
    // only for sending. Not bound to the discovery ports themselves: that
    // would compete with RL's own socket for delivery (see the Windows port's
    // notes). RL joins the ip:port rewritten *inside the payload* (see
    // hosting.rs's rewrite_lan_beacon_payload), not the source port.
    socket: std::net::UdpSocket,
    captured: Receiver<(Vec<u8>, SocketAddr, u16)>,
    capture: Arc<RawCapture>,
}

struct RawCapture {
    fd: OwnedFd,
    stop: AtomicBool,
}

impl RawCapture {
    fn open() -> Result<Self, String> {
        let fd = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                i32::from(ETH_P_IP.to_be()),
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(libc::EPERM) {
                "Hebnix needs the CAP_NET_RAW permission for this (Grant permission in the Multiplayer tab)".to_string()
            } else {
                error.to_string()
            });
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // wake up regularly so stop() is noticed without closing the fd
        // from under a blocked recvfrom
        let timeout = libc::timeval { tv_sec: 0, tv_usec: 200_000 };
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &timeout as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
        }
        Ok(Self { fd, stop: AtomicBool::new(false) })
    }

    /// Ok(None) on a timeout or a packet that wasn't sent by this machine
    fn recv(&self, buffer: &mut [u8]) -> std::io::Result<Option<usize>> {
        let mut address: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        let mut address_len = std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
        let len = unsafe {
            libc::recvfrom(
                self.fd.as_raw_fd(),
                buffer.as_mut_ptr() as *mut libc::c_void,
                buffer.len(),
                0,
                &mut address as *mut _ as *mut libc::sockaddr,
                &mut address_len,
            )
        };
        if len < 0 {
            let error = std::io::Error::last_os_error();
            return match error.kind() {
                std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted => Ok(None),
                _ => Err(error),
            };
        }
        Ok((address.sll_pkttype == PACKET_OUTGOING).then_some(len as usize))
    }

    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl BeaconRelay {
    /// `host_tailnet_ip` is this machine's own tailnet address - the send
    /// socket binds there specifically, on an ephemeral port.
    pub fn bind(host_tailnet_ip: IpAddr) -> Result<Self, String> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, None)
            .map_err(|error| format!("could not create the beacon relay socket: {error}"))?;
        socket
            .set_broadcast(true)
            .map_err(|error| format!("could not enable broadcast on the beacon relay socket: {error}"))?;
        socket
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let address: SocketAddr = (host_tailnet_ip, 0).into();
        socket
            .bind(&address.into())
            .map_err(|error| format!("could not bind the beacon relay socket to {host_tailnet_ip}: {error}"))?;

        let (tx, rx) = mpsc::channel();
        let capture = spawn_capture_thread(tx)?;

        Ok(Self {
            socket: socket.into(),
            captured: rx,
            capture,
        })
    }

    /// non-blocking - returns the next captured beacon packet, if any,
    /// along with the peer that sent it and which discovery port it was on
    pub fn try_receive(&self) -> Option<(Vec<u8>, SocketAddr, u16)> {
        self.captured.try_recv().ok()
    }

    /// stops the capture thread (within one receive timeout)
    pub fn stop_capture(&self) {
        self.capture.shutdown();
    }

    /// sends `payload` to `destination` (the peer's ip and whichever
    /// discovery port the original beacon was captured on)
    pub fn send_to(&self, payload: &[u8], destination: SocketAddr) -> Result<(), String> {
        self.socket
            .send_to(payload, destination)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

impl Drop for BeaconRelay {
    fn drop(&mut self) {
        self.capture.shutdown();
    }
}

/// RL's own discovery broadcast, and only that: outgoing, IPv4 UDP, to
/// 255.255.255.255 on a discovery port. The relay's own sends are unicast
/// to a peer, so they can never loop back in here.
fn is_discovery_broadcast(ip: &[u8], destination_port: u16) -> bool {
    ip.len() >= 20
        && ip[16..20] == [255, 255, 255, 255]
        && super::RL_DISCOVERY_PORTS.contains(&destination_port)
}

fn spawn_capture_thread(tx: Sender<(Vec<u8>, SocketAddr, u16)>) -> Result<Arc<RawCapture>, String> {
    let capture = Arc::new(
        RawCapture::open().map_err(|error| format!("could not start the beacon capture: {error}"))?,
    );

    let thread_capture = capture.clone();
    std::thread::Builder::new()
        .name("beacon-capture".into())
        .spawn(move || {
            // plenty for a LAN discovery beacon
            let mut buffer = vec![0u8; 4096];
            while !thread_capture.stop.load(Ordering::Relaxed) {
                match thread_capture.recv(&mut buffer) {
                    Ok(Some(len)) => {
                        let packet = &buffer[..len];
                        let Some(parsed) = parse_udp_payload(packet) else {
                            continue;
                        };
                        if !is_discovery_broadcast(packet, parsed.2) {
                            continue;
                        }
                        if tx.send(parsed).is_err() {
                            break; // BeaconRelay was dropped
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        })
        .map_err(|error| format!("could not start the beacon capture thread: {error}"))?;

    Ok(capture)
}

/// pulls the UDP payload, source address, and destination port out of a raw
/// IP packet - the SOCK_DGRAM packet socket hands us the packet starting at
/// the IP header (the link-layer header is already stripped), so this is
/// the same job a normal socket's recv_from would otherwise do
fn parse_udp_payload(ip: &[u8]) -> Option<(Vec<u8>, SocketAddr, u16)> {
    const UDP_PROTOCOL: u8 = 17;

    if ip.len() < 20 {
        return None;
    }
    let version = ip[0] >> 4;
    if version != 4 {
        return None;
    }
    let header_len = (ip[0] & 0x0F) as usize * 4;
    if header_len < 20 || ip.len() < header_len + 8 {
        return None;
    }
    if ip[9] != UDP_PROTOCOL {
        return None;
    }
    let source_ip = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);

    let udp = &ip[header_len..];
    let source_port = u16::from_be_bytes([udp[0], udp[1]]);
    let destination_port = u16::from_be_bytes([udp[2], udp[3]]);
    let udp_length = u16::from_be_bytes([udp[4], udp[5]]) as usize;
    if udp_length < 8 || udp.len() < 8 {
        return None;
    }
    let payload_end = udp_length.min(udp.len());
    let payload = udp[8..payload_end].to_vec();

    Some((
        payload,
        SocketAddr::new(source_ip.into(), source_port),
        destination_port,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_udp_payload_out_of_a_raw_ip_packet() {
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45; // version 4, header length 20
        ip[9] = 17; // UDP
        ip[12..16].copy_from_slice(&[10, 242, 77, 1]);
        let mut udp = vec![0u8; 8];
        udp[0..2].copy_from_slice(&14001u16.to_be_bytes());
        udp[2..4].copy_from_slice(&14001u16.to_be_bytes());
        let payload = b"hello beacon";
        udp[4..6].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        udp.extend_from_slice(payload);
        ip.extend_from_slice(&udp);

        let (parsed_payload, source, destination_port) =
            parse_udp_payload(&ip).expect("should parse a well-formed UDP packet");
        assert_eq!(parsed_payload, payload);
        assert_eq!(source, "10.242.77.1:14001".parse().unwrap());
        assert_eq!(destination_port, 14001);
    }

    #[test]
    fn only_broadcasts_to_discovery_ports_count() {
        let mut ip = vec![0u8; 20];
        ip[16..20].copy_from_slice(&[255, 255, 255, 255]);
        assert!(is_discovery_broadcast(&ip, 14001));
        assert!(!is_discovery_broadcast(&ip, 7777));
        ip[16..20].copy_from_slice(&[10, 242, 77, 2]);
        assert!(!is_discovery_broadcast(&ip, 14001));
    }

    #[test]
    fn ignores_non_udp_and_truncated_packets() {
        assert!(parse_udp_payload(&[0u8; 10]).is_none()); // too short for an IP header
        let mut non_udp = vec![0u8; 20];
        non_udp[0] = 0x45;
        non_udp[9] = 6; // TCP, not UDP
        assert!(parse_udp_payload(&non_udp).is_none());
    }
}
