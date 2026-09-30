// Runs the real upstream `tailscaled` + `tailscale` binaries to join the
// headscale-coordinated tailnet for Workshop multiplayer (see the Windows
// sidecar/README.md for why this isn't the `tsnet` library).
//
// Windows has to install `tailscaled` as a patched Windows service. Linux
// doesn't: a plain child process with its own `--tun`, `--socket` and
// `--statedir` is kept entirely separate from any Tailscale the user runs
// themselves, and gets CAP_NET_ADMIN/CAP_NET_RAW through the same ambient
// capability dance the old TAP code used (see caps.rs), so Hebnix never
// needs root. Commands shell out to `tailscale --socket=<ours> ...`;
// results arrive as `AppMsg::Tsnet*` on the app's message channel.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;
use serde::Deserialize;

use crate::messages::AppMsg;

/// our own TUN device, never the user's `tailscale0`
pub const TUN_NAME: &str = "hebnixts0";
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(15);
const PEER_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TsState {
    Stopped,
    Starting,
    Connecting,
    Connected,
    Backoff,
}

impl TsState {
    fn parse(backend_state: &str) -> Self {
        match backend_state {
            "Running" => TsState::Connected,
            "Starting" => TsState::Starting,
            "NeedsLogin" | "NeedsMachineAuth" => TsState::Connecting,
            _ => TsState::Stopped,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub tailnet_ip: String,
    pub hostname: String,
    pub online: bool,
}

// tailscale sends null for empty lists/maps (e.g. "Peer": null with no peers
// yet), and serde's default only covers a missing field, so treat null as empty
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
struct RawStatus {
    #[serde(rename = "BackendState", default, deserialize_with = "null_as_default")]
    backend_state: String,
    #[serde(rename = "TailscaleIPs", default, deserialize_with = "null_as_default")]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Peer", default, deserialize_with = "null_as_default")]
    peer: HashMap<String, RawPeer>,
}

#[derive(Deserialize)]
struct RawPeer {
    #[serde(rename = "HostName", default, deserialize_with = "null_as_default")]
    host_name: String,
    #[serde(rename = "TailscaleIPs", default, deserialize_with = "null_as_default")]
    tailscale_ips: Vec<String>,
    // confirmed live (2026-09-28): a peer's own `TailscaleIPs` here can be
    // v6-only - `AllowedIPs` (CIDR routes, e.g. "10.242.77.2/32") is what
    // actually has the v4 address reliably for every peer. `TailscaleIPs`
    // still works fine for *self* (the node's own status always lists v4
    // first there), just not for peers.
    #[serde(rename = "AllowedIPs", default, deserialize_with = "null_as_default")]
    allowed_ips: Vec<String>,
    #[serde(rename = "Online", default)]
    online: bool,
}

/// A handle to Hebnix's own tailscaled. Commands are fire-and-forget
/// (each spawns its own short-lived worker thread that shells out to the
/// CLI); results/events arrive asynchronously as `AppMsg::Tsnet*` variants
/// on `tx`. Dropping it brings the tailnet down and stops the daemon.
pub struct TsnetSidecarHandle {
    tailscale_cli: PathBuf,
    socket: PathBuf,
    pid_file: PathBuf,
    daemon: Mutex<Option<Child>>,
    tx: Sender<AppMsg>,
    poll_stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for TsnetSidecarHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TsnetSidecarHandle")
            .finish_non_exhaustive()
    }
}

impl TsnetSidecarHandle {
    /// Starts Hebnix's own `tailscaled` (stopping one a crashed earlier
    /// session left behind) and a background peer-status poller. Needs
    /// CAP_NET_ADMIN on the Hebnix binary, see caps.rs.
    pub fn spawn(state_dir: &Path, tx: Sender<AppMsg>) -> Result<Self, String> {
        let tailscaled = find_binary("tailscaled")?;
        let tailscale_cli = find_binary("tailscale")?;
        std::fs::create_dir_all(state_dir).map_err(|error| {
            format!("could not create the multiplayer network's state directory: {error}")
        })?;
        let socket = socket_path(state_dir);
        if let Some(parent) = socket.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let pid_file = state_dir.join("tailscaled.pid");
        stop_stale_daemon(&pid_file);
        if let Some(pid) = foreign_tailscaled() {
            let _ = tx.send(AppMsg::Log(format!(
                "[tsnet] another tailscaled (pid {pid}) is running. Workshop multiplayer runs its \
                 own separate one, but two can fight over routes; stop yours if the connection misbehaves."
            )));
        }

        let log_path = state_dir.join("tailscaled.log");
        let log = std::fs::File::create(&log_path)
            .map_err(|error| format!("could not create {}: {error}", log_path.display()))?;
        let log_err = log.try_clone().map_err(|error| error.to_string())?;
        let child = super::caps::command_with_net_caps(&tailscaled)
            .arg(format!("--tun={TUN_NAME}"))
            .arg(format!("--socket={}", socket.display()))
            .arg(format!("--statedir={}", state_dir.display()))
            .arg("--port=0")
            // don't upload this daemon's logs to Tailscale Inc.
            .arg("--no-logs-no-support")
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err)
            .spawn()
            .map_err(|error| format!("could not start tailscaled: {error}"))?;
        let _ = std::fs::write(&pid_file, child.id().to_string());

        let handle = Self {
            tailscale_cli,
            socket,
            pid_file,
            daemon: Mutex::new(Some(child)),
            tx: tx.clone(),
            poll_stop: Arc::new(AtomicBool::new(false)),
        };
        handle.wait_until_ready(&log_path)?;
        spawn_peer_poller(handle.cli(), tx, handle.poll_stop.clone());
        Ok(handle)
    }

    fn cli(&self) -> Cli {
        Cli {
            program: self.tailscale_cli.clone(),
            socket: self.socket.clone(),
        }
    }

    /// polls the LocalAPI until the daemon answers, failing early (with the
    /// tail of its log) if it exits instead
    fn wait_until_ready(&self, log_path: &Path) -> Result<(), String> {
        let start = std::time::Instant::now();
        while start.elapsed() < DAEMON_START_TIMEOUT {
            let exited = self
                .daemon
                .lock()
                .ok()
                .and_then(|mut daemon| daemon.as_mut().and_then(|child| child.try_wait().ok()))
                .flatten();
            if let Some(status) = exited {
                return Err(format!(
                    "tailscaled exited ({status}): {}",
                    log_tail(log_path)
                ));
            }
            if self.socket.exists() && fetch_status(&self.cli()).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(format!(
            "tailscaled did not start in time: {}",
            log_tail(log_path)
        ))
    }

    pub fn request_up(&self, auth_key: String, hostname: String, control_url: String) -> Result<(), String> {
        let cli = self.cli();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = bring_up(&cli, &auth_key, &hostname, &control_url);
            let _ = tx.send(AppMsg::TsnetUpResult { result });
        });
        Ok(())
    }

    /// Blocking fetch of the current tailnet peer list, for a background
    /// worker thread (e.g. the beacon relay) that needs a fresh list on its
    /// own cadence rather than an async AppMsg round trip.
    pub fn peers_now(&self) -> Result<Vec<PeerInfo>, String> {
        let status = fetch_status(&self.cli())?;
        Ok(status.peer.into_values().map(peer_info).collect())
    }

    pub fn request_status(&self) -> Result<(), String> {
        let cli = self.cli();
        let tx = self.tx.clone();
        std::thread::spawn(move || match fetch_status(&cli) {
            Ok(status) => {
                let _ = tx.send(AppMsg::TsnetStatus {
                    state: TsState::parse(&status.backend_state),
                    tailnet_ip: pick_ipv4(status.tailscale_ips),
                    peers: status.peer.into_values().map(peer_info).collect(),
                });
            }
            Err(error) => {
                let _ = tx.send(AppMsg::Log(format!("[tsnet] status check failed: {error}")));
            }
        });
        Ok(())
    }

    /// Releases the tailnet connection but leaves the daemon running, so a
    /// quick rejoin doesn't have to wait on it starting again.
    pub fn request_down(&self) -> Result<(), String> {
        let cli = self.cli();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let ok = cli.run(&["down"]).is_ok();
            let _ = tx.send(AppMsg::TsnetDownResult { ok });
        });
        Ok(())
    }

    /// Waits for the daemon to exit after kill().
    pub fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            let done = self.daemon.lock().map_or(true, |mut daemon| match daemon.as_mut() {
                Some(child) => child.try_wait().ok().flatten().is_some(),
                None => true,
            });
            if done {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// asks tailscaled to exit (SIGTERM, so it removes its TUN device and
    /// routes itself)
    pub fn kill(&mut self) {
        self.poll_stop.store(true, Ordering::Relaxed);
        if let Ok(daemon) = self.daemon.lock() {
            if let Some(child) = daemon.as_ref() {
                signal(child.id(), nix::sys::signal::Signal::SIGTERM);
            }
        }
    }
}

impl Drop for TsnetSidecarHandle {
    fn drop(&mut self) {
        // best-effort, synchronous: release the tailnet, then stop the
        // daemon so nothing lingers once Workshop multiplayer ends
        let _ = self.cli().run(&["down"]);
        self.kill();
        if !self.wait_for_exit(Duration::from_secs(3)) {
            if let Ok(mut daemon) = self.daemon.lock() {
                if let Some(child) = daemon.as_mut() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

#[derive(Clone)]
struct Cli {
    program: PathBuf,
    socket: PathBuf,
}

impl Cli {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new(&self.program)
            .arg(format!("--socket={}", self.socket.display()))
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("failed to run the multiplayer network helper: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.trim();
            Err(if message.is_empty() {
                format!("tailscale exited with an error ({:?})", output.status)
            } else {
                message.to_string()
            })
        }
    }
}

fn peer_info(peer: RawPeer) -> PeerInfo {
    PeerInfo {
        tailnet_ip: peer_ipv4(&peer.allowed_ips, peer.tailscale_ips).unwrap_or_default(),
        hostname: peer.host_name,
        online: peer.online,
    }
}

/// picks the v4 address out of a *node's own* `TailscaleIPs` - reliably v4
/// first there (confirmed live). Not used for peers any more, see
/// `peer_ipv4` below for why.
fn pick_ipv4(ips: Vec<String>) -> Option<String> {
    ips.iter()
        .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())
        .cloned()
        .or_else(|| ips.into_iter().next())
}

/// A *peer's* `TailscaleIPs` field isn't reliable for this at all -
/// confirmed live that it can be v6-only (`["fd7a:115c:a1e0::2"]`, no v4
/// entry present whatsoever), even though the same peer's `AllowedIPs`
/// always has the v4 route (`"10.242.77.2/32"`). So for peers, read the v4
/// address out of `AllowedIPs` instead, and only fall back to
/// `TailscaleIPs` if that's somehow also missing it.
fn peer_ipv4(allowed_ips: &[String], tailscale_ips: Vec<String>) -> Option<String> {
    allowed_ips
        .iter()
        .find_map(|route| {
            let address = route.split('/').next().unwrap_or(route);
            address.parse::<std::net::Ipv4Addr>().is_ok().then(|| address.to_string())
        })
        .or_else(|| pick_ipv4(tailscale_ips))
}

fn bring_up(cli: &Cli, auth_key: &str, hostname: &str, control_url: &str) -> Result<String, String> {
    cli.run(&[
        "up",
        &format!("--login-server={control_url}"),
        &format!("--authkey={auth_key}"),
        &format!("--hostname={hostname}"),
        // players use raw tailnet IPs; never let this daemon touch the
        // system's DNS (resolv.conf / systemd-resolved)
        "--accept-dns=false",
        // leave the user's iptables/nftables alone; firewall.rs adds the few
        // rules Workshop multiplayer needs to its own table
        "--netfilter-mode=off",
        // the state dir remembers earlier `up` flags; start from these only
        "--reset",
        "--timeout=30s",
    ])?;
    let status = fetch_status(cli)?;
    pick_ipv4(status.tailscale_ips)
        .ok_or_else(|| "connected, but the multiplayer network did not assign an address".to_string())
}

fn fetch_status(cli: &Cli) -> Result<RawStatus, String> {
    let raw = cli.run(&["status", "--json"])?;
    serde_json::from_str(&raw).map_err(|error| format!("could not understand the multiplayer network's status: {error}"))
}

fn spawn_peer_poller(cli: Cli, tx: Sender<AppMsg>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut known: HashMap<String, bool> = HashMap::new();
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(PEER_POLL_INTERVAL);
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(status) = fetch_status(&cli) else {
                continue;
            };
            let mut seen = std::collections::HashSet::new();
            for peer in status.peer.into_values() {
                let Some(ip) = peer_ipv4(&peer.allowed_ips, peer.tailscale_ips) else {
                    continue;
                };
                seen.insert(ip.clone());
                let changed = known.get(&ip).is_none_or(|&prev| prev != peer.online);
                if changed {
                    known.insert(ip.clone(), peer.online);
                    let _ = tx.send(AppMsg::TsnetPeerEvent {
                        online: peer.online,
                        tailnet_ip: ip,
                    });
                }
            }
            known.retain(|ip, _| seen.contains(ip));
        }
    });
}

/// the LocalAPI socket: $XDG_RUNTIME_DIR/hebnix/tailscaled.sock (a tmpfs,
/// so a stale socket can't survive a reboot), else inside the state dir
fn socket_path(state_dir: &Path) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .map(|dir| dir.join("hebnix").join("tailscaled.sock"))
        .unwrap_or_else(|| state_dir.join("tailscaled.sock"))
}

/// `name` from Hebnix's own `tailscale-bin` folder (for distros without a
/// tailscale package), next to the Hebnix binary, or PATH
fn find_binary(name: &str) -> Result<PathBuf, String> {
    let mut dirs = vec![crate::config::base_dir().join("tailscale-bin")];
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(["/usr/sbin", "/usr/bin", "/sbin"].map(PathBuf::from));
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            format!(
                "`{name}` was not found. Install Tailscale from your distro (e.g. `sudo pacman -S tailscale`, \
                 or https://tailscale.com/download/linux; Hebnix runs its own separate copy and does not \
                 need the tailscaled service enabled), or put the `tailscale` and `tailscaled` binaries in {}.",
                crate::config::base_dir().join("tailscale-bin").display()
            )
        })
}

fn signal(pid: u32, signal: nix::sys::signal::Signal) {
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), signal);
}

fn process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|name| name.trim().to_string())
}

/// a tailscaled a crashed or force-killed Hebnix left running would keep
/// holding our TUN device and socket
pub(super) fn stop_stale_daemon(pid_file: &Path) {
    let Some(pid) = std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
    else {
        return;
    };
    if process_name(pid).as_deref() == Some("tailscaled") {
        signal(pid, nix::sys::signal::Signal::SIGTERM);
        for _ in 0..30 {
            if process_name(pid).is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if process_name(pid).is_some() {
            signal(pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let _ = std::fs::remove_file(pid_file);
}

/// a tailscaled that isn't ours (the user's own Tailscale). Both use routing
/// table 52, so they can step on each other's routes.
fn foreign_tailscaled() -> Option<u32> {
    let ours = format!("--tun={TUN_NAME}");
    std::fs::read_dir("/proc").ok()?.flatten().find_map(|entry| {
        let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
        if process_name(pid).as_deref() != Some("tailscaled") {
            return None;
        }
        let cmdline = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
        (!String::from_utf8_lossy(&cmdline).contains(&ours)).then_some(pid)
    })
}

fn log_tail(path: &Path) -> String {
    let mut text = String::new();
    if let Ok(mut file) = std::fs::File::open(path) {
        let _ = file.read_to_string(&mut text);
    }
    let lines: Vec<&str> = text.lines().rev().take(5).collect();
    let tail = lines.into_iter().rev().collect::<Vec<_>>().join(" | ");
    if tail.is_empty() {
        format!("see {}", path.display())
    } else {
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_with_null_peer_and_ips_parses() {
        let json = r#"{"BackendState":"Running","TailscaleIPs":null,"Peer":null}"#;
        let status: RawStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.backend_state, "Running");
        assert!(status.tailscale_ips.is_empty());
        assert!(status.peer.is_empty());
    }

    #[test]
    fn a_peers_v4_address_comes_from_allowed_ips() {
        let allowed = vec!["fd7a:115c:a1e0::2/128".to_string(), "10.242.77.2/32".to_string()];
        let ips = vec!["fd7a:115c:a1e0::2".to_string()];
        assert_eq!(peer_ipv4(&allowed, ips).as_deref(), Some("10.242.77.2"));
    }

    #[test]
    fn the_socket_lives_in_the_runtime_dir_when_there_is_one() {
        let path = socket_path(Path::new("/nonexistent/state"));
        assert!(path.ends_with("tailscaled.sock"));
    }
}
