//! Linux capabilities for Workshop LAN multiplayer and the spoofer.
//!
//! Windows gates these features on running as administrator. Linux never
//! elevates the app itself: `setcap` on the binary (done once, see
//! grant_via_pkexec) grants CAP_NET_ADMIN (tailscaled's TUN device and
//! routes, nftables), CAP_NET_RAW (the beacon capture socket) and
//! CAP_NET_BIND_SERVICE (spoofer's port-443 proxy).

use std::process::Command;

// per linux/capability.h
const CAP_NET_BIND_SERVICE: u32 = 10;
const CAP_NET_ADMIN: u32 = 12;
const CAP_NET_RAW: u32 = 13;

/// `setcap cap_net_admin+eip` on the binary only grants CAP_NET_ADMIN to
/// *this* process - it does not propagate to child processes spawned via
/// fork+exec (e.g. `ip`, `nft`), which normally start with a clean
/// capability set regardless of what the parent has.
///
/// Promotes CAP_NET_ADMIN into our own Inheritable set (a process can always
/// add one of its own Permitted caps to its own Inheritable set - self-
/// modification, no extra privilege needed) - a prerequisite for
/// `command_with_net_admin` below, which raises the capability into each
/// `ip`/`nft` child's own Ambient set individually rather than our own.
///
/// Deliberately does NOT raise our own process's Ambient set globally (an
/// earlier version of this did, and it broke every Heroic-launched game:
/// Ambient is inherited by *every* descendant indefinitely, and bubblewrap
/// - used by Steam Linux Runtime's sandboxing, several forks down from
/// Heroic - treats an unexpected inherited capability as a sandbox-escape
/// red flag and refuses to run at all ("Unexpected capabilities but not
/// setuid, old file caps config?", verified live). Call this once at
/// startup.
pub fn raise_net_admin_ambient() {
    use caps::{CapSet, Capability};
    // best-effort: silently no-ops if CAP_NET_ADMIN isn't in this process's
    // Permitted set at all yet (e.g. a plain `cargo run` dev binary with no
    // setcap applied), or if running as root (which needs no capability
    // dance in the first place).
    let _ = caps::raise(None, CapSet::Inheritable, Capability::CAP_NET_ADMIN);
    // tailscaled (see tsnet_sidecar.rs) also gets CAP_NET_RAW the same way
    let _ = caps::raise(None, CapSet::Inheritable, Capability::CAP_NET_RAW);
}

/// builds a `Command` for `program` that raises CAP_NET_ADMIN into *this one
/// child's* own Ambient set (via a pre_exec hook, running after fork but
/// before exec) rather than our own process's - see raise_net_admin_ambient
/// above for why that distinction matters. Requires raise_net_admin_ambient
/// to have already promoted CAP_NET_ADMIN into our own Inheritable set,
/// since fork() duplicates the parent's full capability state (Permitted,
/// Inheritable, ...) into the child before this hook runs, and the Ambient
/// raise itself needs the capability in both Permitted and Inheritable.
pub(super) fn command_with_net_admin(program: &str) -> Command {
    command_with_caps(program, &[CAP_NET_ADMIN])
}

/// like command_with_net_admin, for tailscaled: it needs CAP_NET_ADMIN for
/// its TUN device and routes, and CAP_NET_RAW for its raw sockets. A cap
/// that isn't in our Permitted+Inheritable sets is silently skipped (the
/// raise just fails), so an older grant without CAP_NET_RAW still starts it.
pub(super) fn command_with_net_caps(program: &std::path::Path) -> Command {
    command_with_caps(program, &[CAP_NET_ADMIN, CAP_NET_RAW])
}

fn command_with_caps(program: impl AsRef<std::ffi::OsStr>, caps: &[u32]) -> Command {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(program);
    let caps = caps.to_vec();
    unsafe {
        cmd.pre_exec(move || {
            const PR_CAP_AMBIENT: libc::c_int = 47;
            const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;
            for &cap in &caps {
                libc::prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, cap as libc::c_ulong, 0, 0);
            }
            Ok(())
        });
    }
    cmd
}

/// Checks the process's effective capability set directly rather than
/// reusing the (root-only) admin check the spoofer feature uses.
fn has_effective_capability(bit: u32) -> bool {
    // root implicitly has every capability, including a plain `cargo run`
    // during development where setcap was never applied to the debug binary
    if nix::unistd::geteuid().is_root() {
        return true;
    }
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    let Some(line) = status.lines().find(|line| line.starts_with("CapEff:")) else {
        return false;
    };
    let Some(hex) = line.split_whitespace().nth(1) else {
        return false;
    };
    let Ok(mask) = u64::from_str_radix(hex, 16) else {
        return false;
    };
    mask & (1 << bit) != 0
}

pub fn has_net_admin_capability() -> bool {
    has_effective_capability(CAP_NET_ADMIN)
}

/// the beacon capture (see beacon.rs) is a raw AF_PACKET socket
pub fn has_net_raw_capability() -> bool {
    has_effective_capability(CAP_NET_RAW)
}

/// everything Workshop multiplayer needs
pub fn has_multiplayer_capabilities() -> bool {
    has_net_admin_capability() && has_net_raw_capability()
}

/// spoofer's local MITM proxy binds 127.0.0.1:443 directly (see
/// spoofer::socket::REVERSE_ADDR) - a privileged port, needing this same
/// capability (or root). Granted by the same grant_via_pkexec() call as
/// CAP_NET_ADMIN, one setcap covering both.
pub fn has_net_bind_service_capability() -> bool {
    has_effective_capability(CAP_NET_BIND_SERVICE)
}

/// grants this binary every capability it can ever need (CAP_NET_ADMIN and
/// CAP_NET_RAW for Workshop multiplayer, CAP_NET_BIND_SERVICE for spoofer's
/// port-443 bind) via a graphical PolicyKit prompt (the Linux equivalent of
/// a Windows UAC dialog) instead of making the user open a terminal.
/// Blocks until the user responds to the dialog - call from a background
/// thread, not the UI thread. The *currently running* process can't pick
/// up a capability granted to its own file after the fact (capabilities
/// are fixed at exec() time) - the caller needs to relaunch Hebnix for it
/// to take effect, same as any setcap change.
/// the `setcap` capability list, also shown in the manual instructions
pub const GRANTED_CAPS: &str = "cap_net_admin,cap_net_raw,cap_net_bind_service=eip";

pub fn grant_via_pkexec() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not find Hebnix's own binary path: {error}"))?;
    let output = Command::new("pkexec")
        .args(["setcap", GRANTED_CAPS])
        .arg(&exe)
        .output()
        .map_err(|error| format!("could not run pkexec: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "pkexec was cancelled or denied".to_string()
        } else {
            stderr
        })
    }
}
