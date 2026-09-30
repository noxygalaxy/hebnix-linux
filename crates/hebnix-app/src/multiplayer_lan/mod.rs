//! Workshop LAN multiplayer over a private tailnet.
//!
//! Every player runs Hebnix's own `tailscaled` (tsnet_sidecar.rs) joined to
//! a headscale server, starts Rocket League with `-multihome=<tailnet ip>`,
//! and the beacon relay (beacon.rs, hosting.rs) forwards RL's LAN discovery
//! broadcast to every peer. Linux needs no root: the Hebnix binary gets
//! CAP_NET_ADMIN/CAP_NET_RAW via `setcap` (caps.rs), and firewall rules go
//! into Hebnix's own nftables table (firewall.rs).

mod beacon;
mod caps;
mod direct_udp;
mod firewall;
mod hosting;
mod map_sync;
mod models;
mod room_api;
mod tsnet_sidecar;

use std::time::Duration;

pub use direct_udp::TunnelStats;
pub use firewall::{
    ensure_beacon_relay_rule, ensure_map_sync_rule, ensure_rocket_league_lan_rule,
    ensure_sidecar_rule,
};
// "host" and "join" only mean something inside Rocket League's own UI now -
// every peer runs the same relay (see hosting.rs), so there's just the one
// session type
pub use hosting::HostSession;
pub use map_sync::{
    LocalInfo, MAP_SYNC_PORT, MAX_MAP_BYTES, MapFileProvider, MapProvider, SlotMap,
    TransferProgress, fetch_map_file, hash_file, is_local_map_id, local_map_id, valid_map_id,
};
pub use models::{
    CreateRoomRequest, JoinRoomRequest, JoinedRoom, LeaveRoomRequest, MapDescriptor, Room,
    RoomCredentials, TsnetAuthKey, UpdatePlayerRequest,
};
pub use room_api::RoomClient;
pub use tsnet_sidecar::{PeerInfo, TsState, TsnetSidecarHandle};
pub use caps::{
    GRANTED_CAPS, grant_via_pkexec, has_multiplayer_capabilities,
    has_net_bind_service_capability, raise_net_admin_ambient,
};

/// Where the headscale coordination server for Workshop LAN lives. Harry
/// said he'll likely just run headscale on the existing api.hebnix.com box
/// rather than standing up a separate subdomain, so this points there by
/// default -- update this one constant if he ends up hosting it elsewhere.
pub const TSNET_CONTROL_URL: &str = "https://hs.xplodingeggo.space"; // TEST ONLY, do not commit
pub const ROOM_API_BASE_URL: &str = "https://hs.xplodingeggo.space"; // TEST ONLY, do not commit

/// Rocket League's actual game traffic port. This is the value rewritten
/// *inside* the beacon payload (the "join me at ip:port" the packet
/// advertises) and what guests' `-multihome` sockets receive game traffic
/// on -- Hebnix never relays this port directly, RL talks it peer to peer
/// once `-multihome` is set.
pub const RL_LAN_PORT: u16 = 7777;

/// Rocket League's LAN *discovery* broadcast ports -- confirmed empirically
/// against a real match that a single port (14777 alone) isn't enough, RL
/// spreads its discovery beacon across 14000-14010 too. Distinct from
/// RL_LAN_PORT above (the port advertised inside that beacon's payload for
/// the real game connection) -- the beacon relay binds one socket per port
/// here and relays whatever it captures back out on the same port.
pub const RL_DISCOVERY_PORTS: &[u16] = &[
    14000, 14001, 14002, 14003, 14004, 14005, 14006, 14007, 14008, 14009, 14010, 14777,
];

pub const PACKET_PUMP_INTERVAL: Duration = Duration::from_millis(50);
/// how often the beacon relay re-fetches the tailnet's peer list to know
/// who to relay captured beacons to. This used to be 300s, left over from
/// when it was a room heartbeat rather than "who's actually online right
/// now" -- confirmed live that this left the relay with an empty peer list
/// (and so nothing to relay to) for the first 5 minutes of every session.
/// The peer list is also fetched once up front before the relay loop
/// starts at all, rather than waiting for the first tick of this interval.
pub const PEER_REFRESH_INTERVAL: Duration = Duration::from_secs(3);
/// how long a session stays alive after Rocket League exits before Hebnix
/// tears the room/tailnet down for real -- covers an ordinary crash/restart
/// without kicking everyone out of the room.
pub const CRASH_GRACE_WINDOW: Duration = Duration::from_secs(90);

/// Hebnix's tailscaled state (node key, logs, pid file)
pub fn tsnet_state_dir() -> std::path::PathBuf {
    crate::config::base_dir()
        .join("multiplayer-lan")
        .join("tsnet-state")
}

pub fn cleanup_system_state() -> Result<(), String> {
    // a tailscaled a crashed session left running keeps its TUN device up
    tsnet_sidecar::stop_stale_daemon(&tsnet_state_dir().join("tailscaled.pid"));
    firewall::remove_rules()
}
