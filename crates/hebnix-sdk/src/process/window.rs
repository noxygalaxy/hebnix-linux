//! RL window focus + geometry, via Hyprland's IPC socket or (best-effort,
//! untested live -- see module note below) KWin/Plasma's `kdotool` +
//! `kscreen-doctor`.
//!
//! Hyprland exposes a plain-text-request/JSON-response unix socket at
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`.
//! Sending `j/activewindow` or `j/clients` returns JSON describing windows,
//! keyed by process pid (`pid` field) -- so like the windows version we
//! identify RL by pid, not by title.
//!
//! KDE Plasma/KWin has no equivalent single IPC socket. The closest
//! analogue is `kdotool` (https://github.com/jinliu/kdotool, packaged as
//! `kdotool-bin` on the AUR) -- an xdotool-alike that, on each invocation,
//! generates a small KWin script, loads+runs+unloads it over KWin's own
//! scripting D-Bus interface, and prints results to stdout. Monitor
//! geometry goes through `kscreen-doctor -j` (ships with Plasma) instead,
//! since kdotool has no screen-geometry command.
//!
//! IMPORTANT: the KWin path below was written from kdotool's and
//! kscreen-doctor's own source (to get exact output formats right) but has
//! never been run against a real Plasma session -- there is no KDE install
//! to test against here. It should be treated as a first draft: expect to
//! need a few rounds of "here's what broke" from someone actually running
//! Plasma. Two things it deliberately does NOT attempt, for lack of a
//! verifiable mechanism via kdotool's builtin actions: exempting our own
//! window from blur/shadow effects, and forcing it to start floating (both
//! Hyprland-only niceties -- on KWin those functions are no-ops, so at
//! worst the window looks like an ordinary KWin window rather than
//! matching the Windows app's chromeless-floating look).
//!
//! On any other (wlroots or not, non-KDE) compositor, neither
//! `HYPRLAND_INSTANCE_SIGNATURE` nor a KDE session is detected, and we fall
//! back to an "always focused, geometry unknown" stub so the app doesn't
//! crash -- a warning is logged once.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Compositor {
    Hyprland,
    Kwin,
    Other,
}

fn compositor() -> Compositor {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        Compositor::Hyprland
    } else if std::env::var("XDG_CURRENT_DESKTOP")
        .map(|d| d.to_uppercase().contains("KDE"))
        .unwrap_or(false)
        || std::env::var_os("KDE_FULL_SESSION").is_some()
    {
        Compositor::Kwin
    } else {
        Compositor::Other
    }
}

fn warn_unsupported_compositor_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        tracing::warn!(
            "process::window: neither Hyprland nor KDE detected, window focus/geometry \
             tracking is unavailable -- assuming RL is always focused with unknown geometry"
        );
    });
}

fn warn_kdotool_missing_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        tracing::warn!(
            "process::window: KDE detected but `kdotool` isn't on PATH -- window focus/geometry \
             tracking needs it (AUR: kdotool-bin). Falling back to \"always focused\"."
        );
    });
}

// ===================== Hyprland backend =====================

fn hyprland_socket_path() -> Option<std::path::PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime_dir =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
    Some(std::path::PathBuf::from(runtime_dir).join("hypr").join(sig).join(".socket.sock"))
}

/// send a plain-text hyprctl command over the IPC socket, returning the raw
/// response bytes. `j/<cmd>` gets JSON back.
fn hyprctl_raw(cmd: &str) -> Option<Vec<u8>> {
    let path = hyprland_socket_path()?;
    let mut stream = UnixStream::connect(&path).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();
    stream.write_all(cmd.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn hyprctl_json(cmd: &str) -> Option<Value> {
    let raw = hyprctl_raw(&format!("j/{cmd}"))?;
    serde_json::from_slice(&raw).ok()
}

/// is the given layer-shell surface (matched by its `namespace`, e.g.
/// "waybar") currently visible (alpha > 0)? `None` if it isn't found at
/// all (compositor other than Hyprland, or the surface isn't mapped).
///
/// Exists because toggling something via a stateless signal (like waybar's
/// SIGUSR1 hide toggle) and tracking "hidden or not" with our own boolean
/// desyncs the moment *anything else* also toggles it -- the user's own
/// Hyprland keybind for the same signal, in this case. Querying the real
/// state each time and only sending the toggle when it actually needs to
/// change is self-correcting regardless of what else touches it.
pub fn hyprland_layer_visible(namespace: &str) -> Option<bool> {
    let json = hyprctl_json("layers")?;
    let monitors = json.as_object()?;
    for mon in monitors.values() {
        let levels = mon.get("levels")?.as_object()?;
        for layer_arr in levels.values() {
            for layer in layer_arr.as_array()? {
                if layer.get("namespace").and_then(Value::as_str) == Some(namespace) {
                    return Some(layer.get("alpha").and_then(Value::as_f64).unwrap_or(0.0) > 0.0);
                }
            }
        }
    }
    None
}

#[derive(Debug, Deserialize, Default)]
struct HyprClient {
    #[serde(default)]
    pid: i64,
    #[serde(default)]
    at: Option<[i32; 2]>,
    #[serde(default)]
    size: Option<[i32; 2]>,
    #[serde(default)]
    title: String,
}

/// RL's pid via Hyprland's own window list, matched by title rather than
/// process name. Under Proton, `sysinfo`'s process scan is unreliable here
/// -- it occasionally returns a short-lived pid (looks like a worker
/// thread's tid surfacing as a "process") that also happens to share
/// RocketLeague.exe's comm name, causing the real window to intermittently
/// read as unfocused. Hyprland already knows which pid owns which window,
/// so trust that instead of re-deriving it from a process name scan.
fn hyprland_rl_pid() -> Option<u32> {
    let json = hyprctl_json("clients")?;
    let arr = json.as_array()?;
    arr.iter()
        .find(|c| {
            c.get("title")
                .and_then(Value::as_str)
                .map(|t| t.to_lowercase().contains("rocket league"))
                .unwrap_or(false)
        })
        .and_then(|c| c.get("pid"))
        .and_then(Value::as_i64)
        .map(|p| p as u32)
}

/// which workspace RL's own window currently lives on (Hyprland-only).
fn hyprland_rl_workspace(pid: u32) -> Option<i64> {
    let json = hyprctl_json("clients")?;
    let arr = json.as_array()?;
    arr.iter().find(|c| c.get("pid").and_then(Value::as_i64) == Some(pid as i64))
        .and_then(|c| c.get("workspace"))
        .and_then(|w| w.get("id"))
        .and_then(Value::as_i64)
}

// ===================== KWin backend (kdotool + kscreen-doctor) =====================

/// run kdotool, returning stdout lines (trimmed, empty lines dropped) on a
/// zero exit status. `None` (rather than panicking or spamming) if
/// kdotool isn't installed or the call otherwise fails -- callers treat
/// that the same as "no window found".
fn kdotool_lines(args: &[&str]) -> Option<Vec<String>> {
    let output = match std::process::Command::new("kdotool").args(args).output() {
        Ok(o) => o,
        Err(_) => {
            warn_kdotool_missing_once();
            return None;
        }
    };
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(
        stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn kdotool_line(args: &[&str]) -> Option<String> {
    kdotool_lines(args)?.into_iter().next()
}

/// RL's pid via kdotool, matched by window caption the same way the
/// Hyprland path matches by title. `search --name` takes a regex against
/// the window caption; "Rocket League" has no special-regex characters so
/// no escaping is needed. `--limit 1` keeps this to the first match;
/// chaining `getwindowpid` onto the same invocation runs it against
/// whatever `search` put on the window stack, xdotool-style.
fn kwin_rl_pid() -> Option<u32> {
    kdotool_line(&["search", "--name", "Rocket League", "--limit", "1", "getwindowpid"])?
        .parse()
        .ok()
}

/// (left, top, right, bottom) via `search --name <title> --limit 1
/// getwindowgeometry`, which prints exactly three lines:
///   Window {id}
///     Position: x,y
///     Geometry: WxH
///
/// Takes the window by title, not pid: `search --pid <pid>` was confirmed
/// live to ignore the pid filter entirely on this kdotool build (returns
/// *every* window in the session regardless of which pid is given), so
/// this used to silently grab whatever window happened to come first in
/// kdotool's own listing order - anything from Steam to a file manager,
/// not necessarily Rocket League at all.
fn kwin_rl_window_rect() -> Option<(i32, i32, i32, i32)> {
    let lines =
        kdotool_lines(&["search", "--name", "Rocket League", "--limit", "1", "getwindowgeometry"])?;
    let pos_line = lines.iter().find(|l| l.contains("Position:"))?;
    let geo_line = lines.iter().find(|l| l.contains("Geometry:"))?;
    let (x, y) = pos_line.split("Position:").nth(1)?.trim().split_once(',')?;
    let (w, h) = geo_line.split("Geometry:").nth(1)?.trim().split_once('x')?;
    let (left, top) = (x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?);
    let (width, height) = (w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?);
    let (right, bottom) = (left + width, top + height);
    if right > left && bottom > top {
        Some((left, top, right, bottom))
    } else {
        None
    }
}

fn kscreen_doctor_json() -> Option<Value> {
    let output = std::process::Command::new("kscreen-doctor").arg("-j").output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

// ===================== shared cache + dispatch =====================

/// cached RL pid, the process scan / kdotool round-trip costs real time and
/// the focus helpers run several times a second.
fn cached_rl_pid() -> Option<u32> {
    static CACHE: Mutex<Option<(Instant, Option<u32>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((ts, pid)) = *cache {
        if ts.elapsed() < Duration::from_secs(3) {
            return pid;
        }
    }
    let pid = match compositor() {
        Compositor::Hyprland => hyprland_rl_pid(),
        Compositor::Kwin => kwin_rl_pid(),
        Compositor::Other => None,
    }
    .or_else(|| {
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
        sys.processes()
            .iter()
            .find(|(_, p)| {
                p.name()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("rocketleague")
            })
            .map(|(pid, _)| pid.as_u32())
    });
    *cache = Some((Instant::now(), pid));
    pid
}

fn hyprland_active() -> bool {
    compositor() == Compositor::Hyprland
}

/// there's no real HWND concept on wayland; we use RL's pid as a stand-in
/// "handle" so callers that just need an opaque identity still work.
pub fn rocket_league_hwnd() -> Option<u32> {
    cached_rl_pid()
}

/// does the RL window have focus.
pub fn is_rocket_league_focused() -> bool {
    let Some(pid) = cached_rl_pid() else {
        return false;
    };
    match compositor() {
        Compositor::Hyprland => {
            let Some(json) = hyprctl_json("activewindow") else {
                return false;
            };
            json.get("pid").and_then(Value::as_i64) == Some(pid as i64)
        }
        Compositor::Kwin => {
            let Some(active_pid) = kdotool_line(&["getactivewindow", "getwindowpid"]) else {
                return false;
            };
            active_pid.parse::<u32>().map(|p| p == pid).unwrap_or(false)
        }
        Compositor::Other => {
            warn_unsupported_compositor_once();
            // no window tracking available: assume focused so overlays/binds
            // still work rather than silently going dead.
            true
        }
    }
}

/// does the window owned by `pid` have focus right now. False when the
/// compositor can't say (unlike `is_rocket_league_focused`, which assumes
/// focus there), so callers gating global key reads on this stay inert
/// instead of listening to every keystroke on the desktop.
pub fn is_pid_focused(pid: u32) -> bool {
    match compositor() {
        Compositor::Hyprland => hyprctl_json("activewindow")
            .and_then(|json| json.get("pid").and_then(Value::as_i64))
            == Some(pid as i64),
        Compositor::Kwin => kdotool_line(&["getactivewindow", "getwindowpid"])
            .and_then(|active| active.parse::<u32>().ok())
            == Some(pid),
        Compositor::Other => false,
    }
}

/// (left, top, right, bottom) pixel rect of the RL window
pub fn get_rocket_league_window_rect() -> Option<(i32, i32, i32, i32)> {
    let pid = cached_rl_pid()?;
    match compositor() {
        Compositor::Hyprland => {
            let json = hyprctl_json("clients")?;
            let clients: Vec<HyprClient> = serde_json::from_value(json).ok()?;
            let client = clients.into_iter().find(|c| c.pid == pid as i64)?;
            let at = client.at?;
            let size = client.size?;
            let (left, top) = (at[0], at[1]);
            let (right, bottom) = (left + size[0], top + size[1]);
            if right > left && bottom > top {
                Some((left, top, right, bottom))
            } else {
                None
            }
        }
        Compositor::Kwin => kwin_rl_window_rect(),
        Compositor::Other => {
            warn_unsupported_compositor_once();
            None
        }
    }
}

/// Hyprland-only: exempt our own window from decoration effects some rice
/// setups apply broadly to floating/scratchpad-style windows (blur,
/// dim-around) -- these can render as a huge blurred/dimmed halo behind a
/// transparent egui surface, since the surface has no opaque content to
/// bound the effect against. Also makes the window start floating rather
/// than tiled, matching the reference Windows app's always-floating
/// behavior. Injected live over the IPC socket via `keyword`, the same
/// mechanism `hyprctl keyword ...` uses -- this is runtime compositor
/// state, NOT a write to hyprland.conf, and only matches our own window
/// class, so it can't touch any other app's tiling/scratchpad/dim
/// behavior. Lost on Hyprland reload/restart, which is fine since we
/// re-apply it every time we start.
///
/// No KWin equivalent: kdotool's builtin window-state toggles don't cover
/// blur/shadow/compositing-exemption, and Plasma's window rules for that
/// live in user config rather than anything scriptable live over D-Bus in
/// a way that's been verified here. No-op on KWin.
pub fn exempt_own_window_decorations() {
    if !hyprland_active() {
        return;
    }
    // Hyprland >= 0.53 renamed the old `windowrulev2`/`noblur,class:...`
    // syntax to `windowrule` + `match:class ...`, and boolean decoration
    // toggles now take an explicit value with an underscore
    // (`no_blur 1`, not `noblur`). Also: this must run before our window
    // maps -- these are static rules Hyprland only evaluates at map time,
    // not retroactively against an already-mapped window.
    for rule in [
        "no_blur 1",
        "no_shadow 1",
        "no_dim 1",
        "rounding 0",
        "float 1",
        // the active-window border isn't covered by any of the above --
        // it's a separate decoration with its own field. Left at its
        // default, hiding the window (ViewportCommand::Visible(false))
        // can leave a stray outline of it on screen until something else
        // forces a full repaint (confirmed live: focusing RL doesn't
        // clear it, but toggling RL's own fullscreen does).
        "border_size 0",
    ] {
        hyprctl_raw(&format!("keyword windowrule {rule}, match:class ^(Hebnix)$"));
    }
}

/// belt-and-suspenders alongside `exempt_own_window_decorations`: that one
/// is a static windowrule (matched only at window map time, before we can
/// be sure it actually took). This looks up our own window's live address
/// after it's mapped and re-applies the same overrides directly via
/// `dispatch setprop`, which takes effect immediately regardless of
/// map-time rule evaluation. Call once, shortly after the window is first
/// shown. Hyprland-only, see `exempt_own_window_decorations`.
pub fn reassert_own_window_decorations(own_pid: u32) {
    if !hyprland_active() {
        return;
    }
    let Some(json) = hyprctl_json("clients") else {
        return;
    };
    let Some(arr) = json.as_array() else { return };
    let Some(addr) = arr
        .iter()
        .find(|c| c.get("pid").and_then(Value::as_i64) == Some(own_pid as i64))
        .and_then(|c| c.get("address"))
        .and_then(Value::as_str)
    else {
        return;
    };
    // `setprop`'s property names are a different namespace from
    // `windowrule`'s (confirmed against hyprctl's own usage doc) -- this
    // was previously using the windowrule names (no_blur, no_shadow,
    // no_dim), which aren't valid setprop properties at all, so every call
    // here silently no-op'd. The real names are forcenoblur/forcenoshadow/
    // forcenodim/forcenoborder (booleans) and rounding/bordersize (ints).
    for (prop, val) in [
        ("forcenoblur", 1),
        ("forcenoshadow", 1),
        ("forcenodim", 1),
        ("forcenoborder", 1),
        ("rounding", 0),
    ] {
        hyprctl_raw(&format!("dispatch setprop address:{addr} {prop} {val}"));
    }
}

/// bring our own window to the front, on top of a fullscreened RL, the way
/// BakkesMod's overlay pops up on Windows. No-op (returns false) on any
/// compositor we don't have a real implementation for.
pub fn focus_own_window_over_game(own_pid: u32) -> bool {
    match compositor() {
        Compositor::Hyprland => {
            let selector = format!("pid:{own_pid}");
            if let Some(ws) = cached_rl_pid().and_then(hyprland_rl_workspace) {
                hyprctl_raw(&format!("dispatch movetoworkspacesilent {ws},{selector}"));
            }
            hyprctl_raw(&format!("dispatch setfloating {selector}"));
            hyprctl_raw(&format!("dispatch focuswindow {selector}"));
            hyprctl_raw("dispatch bringactivetotop");
            true
        }
        Compositor::Kwin => {
            // `workspace.activeWindow = window` (what windowactivate does)
            // is documented to switch to whatever virtual desktop the
            // window lives on, same as real xdotool on X11 -- unverified
            // here, but no separate desktop-switch step should be needed.
            //
            // By title, not pid: confirmed live against a real KWin/kdotool
            // session that `search --pid <pid>` ignores the pid filter
            // entirely and returns *every* window in the session (Steam,
            // a file manager, Rocket League, whatever else happened to be
            // open) - chaining windowactivate/windowraise onto that raised
            // and activated all of them in sequence, not specifically our
            // own window, which is exactly the reported symptom (F2 doing
            // nothing useful while focused on the game). "Hebnix" is a
            // fixed, unique window title (see main.rs's with_title), so
            // matching by name sidesteps the broken filter entirely -
            // verified live to return exactly one match.
            kdotool_lines(&["search", "--name", "^Hebnix$", "windowactivate", "windowraise"])
                .is_some()
        }
        Compositor::Other => {
            warn_unsupported_compositor_once();
            false
        }
    }
}

/// un-minimize and activate our own window in place, without the
/// workspace move `focus_own_window_over_game` does. A Wayland client can't
/// un-minimize itself (winit ignores it), and a minimized surface gets no
/// frame callbacks, so the app's own event loop stalls until something
/// outside it brings the window back - this is that something. Hyprland has
/// no minimize at all, so there's nothing to undo there.
pub fn unminimize_own_window() -> bool {
    match compositor() {
        Compositor::Kwin => {
            kdotool_lines(&["search", "--name", "^Hebnix$", "windowactivate", "windowraise"])
                .is_some()
        }
        Compositor::Hyprland | Compositor::Other => false,
    }
}

/// pixel size of the monitor RL is on, falls back to the first monitor
/// reported by the compositor, or a 1920x1080 guess if nothing is
/// available.
pub fn rocket_league_monitor_size() -> (i32, i32) {
    match compositor() {
        Compositor::Hyprland => {
            if let Some(json) = hyprctl_json("monitors") {
                if let Some(arr) = json.as_array() {
                    // prefer the monitor RL's window is actually on
                    if let Some(rect) = get_rocket_league_window_rect() {
                        let (cx, cy) = ((rect.0 + rect.2) / 2, (rect.1 + rect.3) / 2);
                        for mon in arr {
                            let x = mon.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
                            let y = mon.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
                            let w = mon.get("width").and_then(Value::as_i64).unwrap_or(0) as i32;
                            let h = mon.get("height").and_then(Value::as_i64).unwrap_or(0) as i32;
                            if cx >= x && cx < x + w && cy >= y && cy < y + h {
                                return (w, h);
                            }
                        }
                    }
                    if let Some(mon) = arr.first() {
                        let w = mon.get("width").and_then(Value::as_i64).unwrap_or(1920) as i32;
                        let h = mon.get("height").and_then(Value::as_i64).unwrap_or(1080) as i32;
                        return (w, h);
                    }
                }
            }
        }
        Compositor::Kwin => {
            // schema verified against KDE::ConfigSerializer's own source:
            // {"outputs":[{"pos":{"x":,"y":},"size":{"width":,"height":},...}], ...}
            if let Some(json) = kscreen_doctor_json() {
                if let Some(arr) = json.get("outputs").and_then(Value::as_array) {
                    let get_wh = |o: &Value| -> Option<(i32, i32, i32, i32)> {
                        let pos = o.get("pos")?;
                        let size = o.get("size")?;
                        Some((
                            pos.get("x")?.as_i64()? as i32,
                            pos.get("y")?.as_i64()? as i32,
                            size.get("width")?.as_i64()? as i32,
                            size.get("height")?.as_i64()? as i32,
                        ))
                    };
                    if let Some(rect) = get_rocket_league_window_rect() {
                        let (cx, cy) = ((rect.0 + rect.2) / 2, (rect.1 + rect.3) / 2);
                        for out in arr {
                            if let Some((x, y, w, h)) = get_wh(out) {
                                if cx >= x && cx < x + w && cy >= y && cy < y + h {
                                    return (w, h);
                                }
                            }
                        }
                    }
                    for out in arr {
                        if out.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
                            if let Some((_, _, w, h)) = get_wh(out) {
                                return (w, h);
                            }
                        }
                    }
                }
            }
        }
        Compositor::Other => warn_unsupported_compositor_once(),
    }
    (1920, 1080)
}

/// is the cursor inside the RL window.
pub fn is_cursor_inside_rl_window() -> bool {
    let Some((left, top, right, bottom)) = get_rocket_league_window_rect() else {
        return false;
    };
    let (x, y) = match compositor() {
        Compositor::Hyprland => {
            let Some(json) = hyprctl_json("cursorpos") else {
                return false;
            };
            (
                json.get("x").and_then(Value::as_i64).unwrap_or(i64::MIN) as i32,
                json.get("y").and_then(Value::as_i64).unwrap_or(i64::MIN) as i32,
            )
        }
        Compositor::Kwin => {
            // "X=123" / "Y=456" / "SCREEN=0" / "WINDOW=..." (--shell mode),
            // one per line, verified against kdotool's own template source.
            let Some(lines) = kdotool_lines(&["getmouselocation", "--shell"]) else {
                return false;
            };
            let get = |prefix: &str| -> Option<i32> {
                lines
                    .iter()
                    .find_map(|l| l.strip_prefix(prefix))
                    .and_then(|v| v.parse().ok())
            };
            match (get("X="), get("Y=")) {
                (Some(x), Some(y)) => (x, y),
                _ => return false,
            }
        }
        Compositor::Other => return false,
    };
    left <= x && x <= right && top <= y && y <= bottom
}
