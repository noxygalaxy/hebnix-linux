//! app config, stored as config.toml under the XDG config dir. first run
//! imports an old config.ini (python version) if present so settings carry
//! over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// default steam library location for RL on Linux: `~/.steam/steam/steamapps/common/rocketleague`.
/// Not a const since it needs `$HOME` expanded via the `dirs` crate -- callers that used to read
/// a `&str` should call this instead.
pub fn default_rl_path() -> String {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".steam/steam/steamapps/common/rocketleague")
        .to_string_lossy()
        .into_owned()
}

pub fn default_statsapi_path() -> String {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".steam/steam/steamapps/common/rocketleague/TAGame/Config/DefaultStatsAPI.ini")
        .to_string_lossy()
        .into_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowCfg {
    pub width: u32,
    pub height: u32,
    /// last on-screen position - only ever actually restored on X11.
    /// Wayland's xdg_toplevel protocol has no "set position" request at all
    /// on any compositor (Hyprland, Sway, KDE, GNOME included) - placement
    /// is compositor/user-controlled by design, not something client code
    /// can request. Harmless to save regardless; just a no-op on restore
    /// there.
    pub x: Option<i32>,
    pub y: Option<i32>,
}

impl Default for WindowCfg {
    fn default() -> Self {
        Self {
            width: 1250,
            height: 700,
            x: None,
            y: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsCfg {
    pub hotkey: String,
    pub theme: String,
    /// main window bg opacity (0.5-1.0)
    pub window_opacity: f32,
    pub start_in_tray: bool,
    pub rl_path: String,
    pub statsapi_path: String,
    pub suppress_left_alerts: bool,
    pub suppress_fullscreen_warning: bool,
    pub suppress_statsapi_rate_warning: bool,
    /// Publish Hebnix/Rocket League activity to the local Discord client.
    pub discord_rich_presence: bool,
    /// Show presence only while Rocket League is running.
    pub discord_rocket_league_only: bool,
    /// Include the selected live match fields in Rich Presence.
    #[serde(alias = "discord_current_gamemode")]
    pub discord_game_state: bool,
    pub discord_show_score: bool,
    pub discord_show_map: bool,
    pub discord_show_gamemode: bool,
    pub discord_custom_message: String,
}

impl Default for SettingsCfg {
    fn default() -> Self {
        Self {
            hotkey: "f2".to_string(),
            theme: "Dark".to_string(),
            window_opacity: 0.96,
            start_in_tray: false,
            rl_path: default_rl_path(),
            statsapi_path: default_statsapi_path(),
            suppress_left_alerts: false,
            suppress_fullscreen_warning: false,
            suppress_statsapi_rate_warning: false,
            discord_rich_presence: true,
            discord_rocket_league_only: false,
            discord_game_state: true,
            discord_show_score: true,
            discord_show_map: true,
            discord_show_gamemode: true,
            discord_custom_message: "Playing Rocket League".to_string(),
        }
    }
}

/// How Rocket League actually gets launched/restarted on this machine.
/// RL has no official Linux/Steam listing anymore, so there's no single
/// "just call steam://" answer - Steam's `run` verb (the only one that
/// supports passing an extra launch argument, needed for Workshop LAN's
/// -multihome flag) only works for a real, owned Steam catalog listing, not
/// a non-Steam shortcut, and a shortcut's *target* could be anything
/// (Heroic, Lutris, ...). Set once via the Rocket League Launch Setup
/// wizard (Settings tab), re-run any time your setup changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RlLaunchMode {
    /// wizard never completed - restart/Workshop LAN buttons should point
    /// the user at Settings instead of guessing
    #[default]
    Unconfigured,
    /// RL is a real, owned Steam listing (native or Proton) - steam://run
    /// supports passing -multihome=<address> directly
    SteamProton,
    /// a Steam non-Steam-shortcut whose target is Heroic - steam://rungameid
    /// works for a plain restart, but steam://run's argument override
    /// doesn't work on shortcuts at all, so Workshop LAN has to bypass Steam
    /// and call Heroic directly for that one relaunch
    SteamShortcutToHeroic,
    /// Heroic only, no Steam involved at all - every relaunch calls Heroic
    /// directly
    HeroicDirect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RlLaunchCfg {
    pub mode: RlLaunchMode,
    /// SteamProton: RL's real Steam appid (252950). SteamShortcutToHeroic:
    /// the shortcut's computed rungameid (a large synthetic number, see
    /// rl_launch::compute_shortcut_id). Unused for HeroicDirect.
    pub steam_id: String,
    /// path to the Heroic binary (SteamShortcutToHeroic and HeroicDirect only)
    pub heroic_binary: String,
    /// Epic catalog app name - "Sugar" for Rocket League, same for everyone
    pub heroic_app_name: String,
    pub heroic_runner: String,
}

impl Default for RlLaunchCfg {
    fn default() -> Self {
        Self {
            mode: RlLaunchMode::Unconfigured,
            steam_id: "252950".to_string(),
            heroic_binary: "heroic".to_string(),
            heroic_app_name: "Sugar".to_string(),
            heroic_runner: "legendary".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchSource {
    #[default]
    Catalog,
    Custom,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PatcherCfg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_ball: Option<String>,
    pub active_boost: Option<String>,
    pub active_decals: std::collections::HashMap<String, String>,
    pub ball_source: PatchSource,
    pub boost_source: PatchSource,
    pub decal_source: PatchSource,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub window: WindowCfg,
    pub settings: SettingsCfg,
    pub rl_launch: RlLaunchCfg,
    pub patcher: PatcherCfg,
    /// enabled state keyed by plugin slug
    pub plugins: BTreeMap<String, bool>,
}

impl Config {
    /// load config.toml, else import an old config.ini, else defaults
    pub fn load(base_dir: &Path) -> Self {
        let toml_path = base_dir.join("config.toml");
        if let Ok(text) = std::fs::read_to_string(&toml_path) {
            match toml::from_str::<Config>(&text) {
                Ok(cfg) => return cfg,
                Err(e) => tracing::warn!("config.toml is invalid ({e}); using defaults"),
            }
        }

        let ini_path = base_dir.join("config.ini");
        if ini_path.exists() {
            if let Some(cfg) = Self::import_ini(&ini_path) {
                tracing::info!("Imported legacy config.ini");
                let _ = cfg.save(base_dir);
                return cfg;
            }
        }

        let cfg = Config::default();
        let _ = cfg.save(base_dir);
        cfg
    }

    pub fn save(&self, base_dir: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = base_dir.join("config.toml");
        let tmp = base_dir.join("config.toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;

        #[cfg(not(feature = "lite"))]
        // keep active patch state with the game installation
        let game_root = Path::new(&self.settings.rl_path);
        #[cfg(not(feature = "lite"))]
        if game_root.is_dir() {
            let marker = serde_json::json!({
                "game_path": game_root.to_string_lossy(),
                "patcher": &self.patcher,
            });
            let _ = std::fs::write(
                game_root.join("patcher.json"),
                serde_json::to_vec_pretty(&marker).unwrap_or_default(),
            );
        }
        Ok(())
    }

    /// one-time import of the old python config.ini
    fn import_ini(path: &Path) -> Option<Config> {
        let ini = ini::Ini::load_from_file(path).ok()?;
        let mut cfg = Config::default();

        if let Some(win) = ini.section(Some("Window")) {
            if let Some(w) = win.get("width").and_then(|v| v.parse().ok()) {
                cfg.window.width = w;
            }
            if let Some(h) = win.get("height").and_then(|v| v.parse().ok()) {
                cfg.window.height = h;
            }
        }
        if let Some(settings) = ini.section(Some("Settings")) {
            if let Some(v) = settings.get("hotkey") {
                cfg.settings.hotkey = v.to_string();
            }
            if let Some(v) = settings.get("theme") {
                cfg.settings.theme = v.to_string();
            }
            if let Some(v) = settings.get("start_in_tray") {
                cfg.settings.start_in_tray = parse_ini_bool(v, false);
            }
            if let Some(v) = settings.get("rl_path") {
                cfg.settings.rl_path = v.to_string();
            }
            if let Some(v) = settings.get("statsapi_path") {
                cfg.settings.statsapi_path = v.to_string();
            }
        }
        if let Some(plugins) = ini.section(Some("Plugins")) {
            for (name, val) in plugins.iter() {
                cfg.plugins
                    .insert(name.to_string(), parse_ini_bool(val, false));
            }
        }
        Some(cfg)
    }
}

fn parse_ini_bool(v: &str, default: bool) -> bool {
    match v.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => true,
        "false" | "0" | "no" | "off" => false,
        _ => default,
    }
}

/// app root dir: `$XDG_CONFIG_HOME/hebnix` (falling back to `~/.config/hebnix`),
/// or `HEBNIX_BASE_DIR` if set (dev runs / fully portable installs). This is
/// where config.toml, plugins/, themes/, fonts/, and logs all live -- it's
/// created on demand and is never inside a read-only package/AppImage.
pub fn base_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HEBNIX_BASE_DIR") {
        return PathBuf::from(dir);
    }
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hebnix");
    migrate_legacy_base_dir(&dir);
    dir
}

/// Known Hebnix-owned entries that used to live next to the executable.
/// Deliberately an allowlist (not "copy everything beside the exe") so the
/// migration can't accidentally vacuum up an unrelated file -- or the
/// executable itself -- into the new config dir.
const LEGACY_ENTRIES: &[&str] = &[
    "config.toml",
    "config.ini",
    "plugins",
    "themes",
    "fonts",
    "presets",
    "balls",
    "boosts",
    "decals",
    "spoofer",
    "friends.json",
    "spoofer_settings.json",
    "owned_products.json",
    "theme_errors.txt",
    "hebnix.ico",
];

/// One-time migration for installs that predate the switch to the XDG config
/// dir, where base_dir used to be "next to the executable". If the new dir
/// doesn't exist yet but an old exe-adjacent config.toml does, copy the
/// known Hebnix data entries over (copy, not move -- never destroys the old
/// data) so existing users don't lose plugins/themes/settings.
fn migrate_legacy_base_dir(new_dir: &Path) {
    if new_dir.join("config.toml").exists() {
        return;
    }
    let Some(legacy_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    else {
        return;
    };
    if !legacy_dir.join("config.toml").exists() {
        return;
    }
    let _ = std::fs::create_dir_all(new_dir);
    let mut migrated = false;
    for entry in LEGACY_ENTRIES {
        let src = legacy_dir.join(entry);
        if !src.exists() {
            continue;
        }
        let dst = new_dir.join(entry);
        let result = if src.is_dir() {
            copy_dir_recursive(&src, &dst)
        } else {
            std::fs::copy(&src, &dst).map(|_| ())
        };
        match result {
            Ok(()) => migrated = true,
            Err(e) => tracing::warn!("failed to migrate legacy {src:?} to {dst:?}: {e}"),
        }
    }
    if migrated {
        tracing::info!("migrated legacy config dir {legacy_dir:?} to {new_dir:?}");
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PatchSource, PatcherCfg};

    #[test]
    fn older_patcher_config_defaults_sources_to_catalog() {
        let config: PatcherCfg = toml::from_str("active_boost = \"Existing\"").unwrap();

        assert_eq!(config.ball_source, PatchSource::Catalog);
        assert_eq!(config.boost_source, PatchSource::Catalog);
        assert_eq!(config.decal_source, PatchSource::Catalog);
    }

    #[test]
    fn patcher_sources_round_trip_independently() {
        let config = PatcherCfg {
            ball_source: PatchSource::Custom,
            boost_source: PatchSource::Catalog,
            decal_source: PatchSource::Custom,
            ..PatcherCfg::default()
        };

        let encoded = toml::to_string(&config).unwrap();
        let decoded: PatcherCfg = toml::from_str(&encoded).unwrap();

        assert_eq!(decoded.ball_source, PatchSource::Custom);
        assert_eq!(decoded.boost_source, PatchSource::Catalog);
        assert_eq!(decoded.decal_source, PatchSource::Custom);
    }
}
