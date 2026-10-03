//! Workshop LAN multiplayer firewall rules, Linux-native via `nftables`.
//!
//! Windows uses the single authoritative Windows Firewall API (`netsh
//! advfirewall`). Linux has no equivalent single authority - nftables,
//! iptables, firewalld and ufw all coexist, and a separate restrictive base
//! chain from one of those can still drop our traffic regardless of what we
//! add here. This adds an allow rule to our own `inet hebnix` table (the
//! common case: no separate restrictive firewall active), but isn't a
//! substitute for the user allowing the ports themselves if they run one.
//! Requires CAP_NET_ADMIN (same capability tailscaled needs).

use std::path::Path;

const CHAIN_IN: &str = "input";
const LAN_PORTS: &str = "7777-7778, 14000-14010, 14777";
const DISCOVERY_PORTS: &str = "14000-14010, 14777";
/// the headscale server hands out addresses from this range
const TAILNET_SUBNET: &str = "10.242.77.0/24";

const CHAIN_NAT: &str = "postrouting";
const NAT_COMMENT: &str = "hebnix-workshop-multihome-nat";

/// tailscaled only makes outbound connections and receives WireGuard on
/// sockets it opened itself, which conntrack already lets back in.
///
/// Rocket League is started with `-multihome=<tailnet ip>`, so every socket
/// it opens, including the HTTPS ones to PsyNet/EOS, uses that address as its
/// source. The kernel still routes those over the normal default route, with
/// a 10.242.77.x source the router drops, and the game never gets past the
/// start screen. Masquerading that source on its way out of any other
/// interface gives those connections the real address and lets the replies
/// find their way back. Traffic to peers goes out the tailnet interface and
/// is left alone.
pub fn ensure_sidecar_rule(executable: &Path) -> Result<(), String> {
    let _ = executable;
    ensure_internet_nat()
}

fn ensure_internet_nat() -> Result<(), String> {
    ensure_table()?;
    let chain_exists = super::caps::command_with_net_admin("nft")
        .args(["list", "chain", "inet", "hebnix", CHAIN_NAT])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !chain_exists {
        run(&[
            "add", "chain", "inet", "hebnix", CHAIN_NAT, "{", "type", "nat", "hook", "postrouting",
            "priority", "srcnat", ";", "policy", "accept", ";", "}",
        ])?;
    }
    if rule_exists(NAT_COMMENT)? {
        return Ok(());
    }
    let interface = format!("\"{}\"", super::tsnet_sidecar::TUN_NAME);
    let comment = format!("\"{NAT_COMMENT}\"");
    run(&[
        "add", "rule", "inet", "hebnix", CHAIN_NAT, "ip", "saddr", TAILNET_SUBNET, "oifname",
        "!=", &interface, "masquerade", "comment", &comment,
    ])
}

/// relayed beacons from peers arrive on the discovery ports
pub fn ensure_beacon_relay_rule(executable: &Path) -> Result<(), String> {
    ensure_table()?;
    ensure_rule(
        "hebnix-workshop-lan-beacon",
        &format!("ip saddr {TAILNET_SUBNET} udp dport {{ {DISCOVERY_PORTS} }}"),
        executable,
    )
}

/// the map sync service (see map_sync.rs), from tailnet peers only
pub fn ensure_map_sync_rule(executable: &Path) -> Result<(), String> {
    ensure_table()?;
    ensure_rule(
        "hebnix-workshop-map-sync",
        &format!(
            "ip saddr {TAILNET_SUBNET} tcp dport {}",
            super::MAP_SYNC_PORT
        ),
        executable,
    )
}

pub fn ensure_rocket_league_lan_rule(executable: &Path, remote: &str) -> Result<(), String> {
    ensure_table()?;
    let comment = format!("hebnix-workshop-lan-rl-{remote}");
    ensure_rule(
        &comment,
        &format!("ip saddr {remote} udp dport {{ {LAN_PORTS} }}"),
        executable,
    )
}

pub fn remove_rules() -> Result<(), String> {
    // dropping the whole table removes every rule we ever added in one shot
    let _ = super::caps::command_with_net_admin("nft")
        .args(["delete", "table", "inet", "hebnix"])
        .output();
    Ok(())
}

fn ensure_table() -> Result<(), String> {
    let exists = super::caps::command_with_net_admin("nft")
        .args(["list", "table", "inet", "hebnix"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    run(&["add", "table", "inet", "hebnix"])?;
    run(&[
        "add", "chain", "inet", "hebnix", CHAIN_IN, "{", "type", "filter", "hook", "input",
        "priority", "0", ";", "policy", "accept", ";", "}",
    ])
}

fn ensure_rule(comment: &str, matcher: &str, executable: &Path) -> Result<(), String> {
    if rule_exists(comment)? {
        return Ok(());
    }
    let _ = executable; // Linux firewall rules match on traffic shape, not the calling binary
    let mut args: Vec<String> = vec!["add", "rule", "inet", "hebnix", CHAIN_IN]
        .into_iter()
        .map(String::from)
        .collect();
    args.extend(matcher.split_whitespace().map(String::from));
    args.push("accept".to_string());
    args.push("comment".to_string());
    args.push(format!("\"{comment}\""));
    run(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn rule_exists(comment: &str) -> Result<bool, String> {
    let output = super::caps::command_with_net_admin("nft")
        .args(["list", "table", "inet", "hebnix"])
        .output()
        .map_err(|e| format!("could not query nftables: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).contains(comment))
}

fn run(args: &[&str]) -> Result<(), String> {
    let output = super::caps::command_with_net_admin("nft")
        .args(args)
        .output()
        .map_err(|e| format!("could not update nftables: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}
