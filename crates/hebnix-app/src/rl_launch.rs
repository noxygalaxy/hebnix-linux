//! Where Rocket League's "restart"/"restart with -multihome=<address>"
//! commands actually go, based on the persisted RlLaunchCfg (see
//! config.rs). Three real-world setups, since RL has no official
//! Linux/Steam listing anymore:
//!
//! - SteamProton: a real, owned Steam catalog listing. `steam://run`
//!   supports overriding the launch options with an extra argument
//!   directly.
//! - SteamShortcutToHeroic: a Steam non-Steam-shortcut whose target is
//!   Heroic. `steam://rungameid` launches it fine, but `steam://run`'s
//!   argument override flatly refuses non-Steam shortcuts ("Game
//!   configuration unavailable", verified live) - so Workshop LAN's
//!   -multihome relaunch has to bypass Steam and call Heroic directly for
//!   just that one relaunch (no Steam overlay during the hosted session,
//!   but it's the only way to get the extra argument in).
//! - HeroicDirect: no Steam at all, every relaunch calls Heroic directly.

use std::path::PathBuf;

use crate::config::{RlLaunchCfg, RlLaunchMode};

/// a Steam non-Steam-shortcut found in shortcuts.vdf whose target looks like
/// Heroic - offered to the user during setup so they don't have to dig the
/// numeric ID out of a generated .desktop file by hand.
pub struct ShortcutCandidate {
    pub app_name: String,
    pub exe: String,
    pub rungameid: u64,
}

/// Steam's shortcut ID algorithm (reverse-engineered, used by every
/// third-party Steam shortcut tool): crc32(exe + appname), top bit forced
/// set, packed into the upper 32 bits of a 64-bit ID with a fixed
/// 0x02000000 suffix. `exe` must be exactly as stored in shortcuts.vdf,
/// quotes included. Verified live against a real shortcut's actual
/// steam://rungameid/<id> from its generated .desktop file.
pub fn compute_shortcut_id(exe: &str, app_name: &str) -> u64 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(exe.as_bytes());
    hasher.update(app_name.as_bytes());
    let top = (hasher.finalize() as u64) | 0x8000_0000;
    (top << 32) | 0x0200_0000
}

fn shortcuts_vdf_path() -> Option<PathBuf> {
    let steam_userdata = dirs::home_dir()?.join(".local/share/Steam/userdata");
    let entries = std::fs::read_dir(&steam_userdata).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("config/shortcuts.vdf");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// shortcuts.vdf is a simple untyped binary keyed map: each entry is a type
// byte (0x00 nested map, 0x01 string, 0x02 int32, 0x08 map-end) followed by
// a null-terminated key, then the value. No official spec, but this format
// is stable and widely relied on by other Steam shortcut tools.
fn parse_map(data: &[u8], mut i: usize) -> Option<(Vec<(String, VdfValue)>, usize)> {
    let mut entries = Vec::new();
    loop {
        let tag = *data.get(i)?;
        i += 1;
        if tag == 0x08 {
            return Some((entries, i));
        }
        let key_end = i + data[i..].iter().position(|b| *b == 0)?;
        let key = String::from_utf8_lossy(&data[i..key_end]).into_owned();
        i = key_end + 1;
        let value = match tag {
            0x00 => {
                let (nested, next) = parse_map(data, i)?;
                i = next;
                VdfValue::Map(nested)
            }
            0x01 => {
                let end = i + data[i..].iter().position(|b| *b == 0)?;
                let s = String::from_utf8_lossy(&data[i..end]).into_owned();
                i = end + 1;
                VdfValue::Str(s)
            }
            0x02 => {
                let bytes: [u8; 4] = data.get(i..i + 4)?.try_into().ok()?;
                i += 4;
                VdfValue::Int(i32::from_le_bytes(bytes))
            }
            _ => return None,
        };
        entries.push((key, value));
    }
}

enum VdfValue {
    Map(Vec<(String, VdfValue)>),
    Str(String),
    #[allow(dead_code)]
    Int(i32),
}

/// non-Steam shortcuts whose target executable looks like Heroic, for the
/// setup wizard to offer as auto-detected candidates.
pub fn find_heroic_shortcuts() -> Vec<ShortcutCandidate> {
    let Some(path) = shortcuts_vdf_path() else {
        return Vec::new();
    };
    let Ok(data) = std::fs::read(&path) else {
        return Vec::new();
    };
    let Some((root, _)) = parse_map(&data, 0) else {
        return Vec::new();
    };
    let Some((_, VdfValue::Map(shortcuts))) = root.into_iter().find(|(k, _)| k == "shortcuts")
    else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    for (_, entry) in shortcuts {
        let VdfValue::Map(fields) = entry else {
            continue;
        };
        let mut app_name = None;
        let mut exe = None;
        for (key, value) in &fields {
            match (key.as_str(), value) {
                ("AppName", VdfValue::Str(s)) => app_name = Some(s.clone()),
                ("Exe", VdfValue::Str(s)) => exe = Some(s.clone()),
                _ => {}
            }
        }
        let (Some(app_name), Some(exe)) = (app_name, exe) else {
            continue;
        };
        if !exe.to_ascii_lowercase().contains("heroic") {
            continue;
        }
        let rungameid = compute_shortcut_id(&exe, &app_name);
        candidates.push(ShortcutCandidate {
            app_name,
            exe,
            rungameid,
        });
    }
    candidates
}

fn launch_uri(uri: &str) -> Result<(), String> {
    tracing::info!("rl_launch: opening {uri}");
    if uri.starts_with("steam://") {
        match std::process::Command::new("steam").arg(uri).spawn() {
            Ok(_) => return Ok(()),
            Err(error) => tracing::warn!(
                "rl_launch: 'steam' binary spawn failed ({error}), falling back to xdg-open"
            ),
        }
    }
    std::process::Command::new("xdg-open")
        .arg(uri)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            let message = format!("could not open '{uri}' via steam or xdg-open: {error}");
            tracing::warn!("rl_launch: {message}");
            message
        })
}

fn heroic_launch(cfg: &RlLaunchCfg, multihome: Option<&str>) -> Result<(), String> {
    let mut uri = format!(
        "heroic://launch?appName={}&runner={}",
        cfg.heroic_app_name, cfg.heroic_runner
    );
    if let Some(address) = multihome {
        uri.push_str(&format!("&arg=-multihome%3D{address}"));
    }
    tracing::info!(
        "rl_launch: spawning '{}' --no-gui --no-sandbox {uri}",
        cfg.heroic_binary
    );
    std::process::Command::new(&cfg.heroic_binary)
        .args(["--no-gui", "--no-sandbox", &uri])
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            let message = format!("could not run '{}': {error}", cfg.heroic_binary);
            tracing::warn!("rl_launch: {message}");
            message
        })
}

/// where RL is actually installed, straight from the launch config -
/// independent of the live process. Live detection
/// (`process::find_rocket_league()`) needs to read the running exe's path,
/// which EAC blocks almost immediately after launch, so in practice it only
/// ever works in the brief window right at startup; `settings.rl_path` was
/// getting stuck on whatever it last resolved to (or a stale manual value)
/// and never correcting itself once EAC engaged - e.g. staying pointed at
/// an old Steam install while the user actually plays through Heroic,
/// silently breaking Workshop map installs (they'd copy into the Steam
/// copy's CookedPCConsole, which the running Heroic-launched game never
/// reads). This resolves the real install directory from data that doesn't
/// need the live process at all: legendary's own installed.json for
/// Heroic-based modes, Steam's appmanifest for SteamProton.
pub fn resolve_install_root(cfg: &RlLaunchCfg) -> Option<PathBuf> {
    match cfg.mode {
        RlLaunchMode::Unconfigured => None,
        RlLaunchMode::HeroicDirect | RlLaunchMode::SteamShortcutToHeroic => {
            let path =
                dirs::home_dir()?.join(".config/heroic/legendaryConfig/legendary/installed.json");
            let text = std::fs::read_to_string(path).ok()?;
            let json: serde_json::Value = serde_json::from_str(&text).ok()?;
            let install_path = json
                .get(&cfg.heroic_app_name)?
                .get("install_path")?
                .as_str()?;
            Some(PathBuf::from(install_path))
        }
        RlLaunchMode::SteamProton => {
            let id = std::env::var("HEBNIX_RL_APPID").unwrap_or_else(|_| cfg.steam_id.clone());
            for steamapps in hebnix_sdk::process::steam_library_steamapps_dirs() {
                let manifest = steamapps.join(format!("appmanifest_{id}.acf"));
                let Ok(text) = std::fs::read_to_string(&manifest) else {
                    continue;
                };
                // simple keyed text VDF: `"installdir"\t\t"Name"` on its own line
                if let Some(installdir) = text.lines().find_map(|line| {
                    let line = line.trim();
                    let rest = line.strip_prefix("\"installdir\"")?.trim();
                    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
                    Some(inner.to_string())
                }) {
                    return Some(steamapps.join("common").join(installdir));
                }
            }
            None
        }
    }
}

/// plain restart, no Workshop LAN address. `HEBNIX_RL_APPID` overrides
/// `cfg.steam_id` for SteamProton/SteamShortcutToHeroic without needing to
/// re-run the setup wizard.
pub fn restart(cfg: &RlLaunchCfg) -> Result<(), String> {
    tracing::info!("rl_launch: restart() mode={:?}", cfg.mode);
    match cfg.mode {
        RlLaunchMode::Unconfigured => Err(
            "Rocket League launch isn't set up yet - open Settings > Rocket League Launch Setup"
                .to_string(),
        ),
        RlLaunchMode::SteamProton | RlLaunchMode::SteamShortcutToHeroic => {
            let id = std::env::var("HEBNIX_RL_APPID").unwrap_or_else(|_| cfg.steam_id.clone());
            launch_uri(&format!("steam://rungameid/{id}"))
        }
        RlLaunchMode::HeroicDirect => heroic_launch(cfg, None),
    }
}

/// Workshop LAN "restart with -multihome=<address>".
/// `HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE` (with `{multihome}` /
/// `{multihome_encoded}` placeholders, shell-word-split) overrides
/// everything below without needing to re-run the setup wizard.
pub fn restart_multihome(cfg: &RlLaunchCfg, address: &str) -> Result<(), String> {
    tracing::info!(
        "rl_launch: restart_multihome() mode={:?} address={address}",
        cfg.mode
    );
    let raw = format!("-multihome={address}");
    let encoded = format!("-multihome%3D{address}");

    if let Ok(template) = std::env::var("HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE") {
        let rendered = template
            .replace("{multihome_encoded}", &encoded)
            .replace("{multihome}", &raw);
        let parts = shell_words::split(&rendered)
            .map_err(|error| format!("invalid HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE: {error}"))?;
        let (program, args) = parts
            .split_first()
            .ok_or_else(|| "HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE is empty".to_string())?;
        return std::process::Command::new(program)
            .args(args)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string());
    }

    match cfg.mode {
        RlLaunchMode::Unconfigured => Err(
            "Rocket League launch isn't set up yet - open Settings > Rocket League Launch Setup"
                .to_string(),
        ),
        RlLaunchMode::SteamProton => {
            let id = std::env::var("HEBNIX_RL_APPID").unwrap_or_else(|_| cfg.steam_id.clone());
            launch_uri(&format!("steam://run/{id}//{raw}/"))
        }
        RlLaunchMode::SteamShortcutToHeroic | RlLaunchMode::HeroicDirect => {
            heroic_launch(cfg, Some(address))
        }
    }
}
