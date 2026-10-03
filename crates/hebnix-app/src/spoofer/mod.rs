// crates/hebnix-app/src/spoofer/mod.rs
//! name spoofer. our own CA (ca), our own mitm proxy (proxy), spoofs live in rules.

pub mod ca;
pub mod crl;
pub mod dns;
pub mod hosts;
pub mod proxy;
pub mod rules;
pub mod skill_bridge;
pub mod socket;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::spoofer::rules::{
    NameRule, OwnedProductsRule, Rule, TITLE_HOST, TitleRule, TitleSettings, TitleSpoofSettings,
};
use crate::spoofer::skill_bridge::SkillBridge;
use crate::spoofer::socket::SocketProxy;

pub const PROXY_HOST: &str = "127.0.0.1";
pub const PROXY_PORT: u16 = 8080;
pub const MAX_NAME_LENGTH: usize = 32;
// Intercept config for title and rank routing, and Epic API for name/friends.
// PsyNet RPC is reached through config when rank spoofing is enabled.
const REDIRECT_HOSTS: [&str; 2] = ["api.epicgames.dev", TITLE_HOST];

pub fn is_admin() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Exactly two things this whole feature ever needs root for: writing
/// /etc/hosts (the MITM redirect) and trusting the CA system-wide via
/// p11-kit. Everything else - the proxy itself (an unprivileged port),
/// DNS resolution, the rules engine - runs fine as a normal user. This
/// app used to relaunch its *entire self* elevated via pkexec whenever
/// spoofer was enabled (mirroring the Windows build, which really does
/// need to run fully elevated) - on Linux that dragged every child
/// process this app ever spawns into running as root too, which broke
/// in ways that took most of a session to fully chase down: Steam/Heroic
/// refusing to start as root, pkexec stripping HOME/WAYLAND_DISPLAY/etc
/// so Steam-shortcut scanning and even the display itself silently broke,
/// and a race against the app's own single-instance lock. All of that
/// goes away by never elevating the app itself at all - instead, each of
/// these two actions runs as its own tiny one-shot `pkexec` call
/// (`run_privileged`, dispatched back into this same binary via
/// `maybe_handle_privileged_cli` - see main()), and the app stays running
/// as the real user the entire time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivilegedAction {
    SetHosts,
    ClearHosts,
    InstallCa,
    UninstallCa,
}

impl PrivilegedAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::SetHosts => "set-hosts",
            Self::ClearHosts => "clear-hosts",
            Self::InstallCa => "install-ca",
            Self::UninstallCa => "uninstall-ca",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "set-hosts" => Self::SetHosts,
            "clear-hosts" => Self::ClearHosts,
            "install-ca" => Self::InstallCa,
            "uninstall-ca" => Self::UninstallCa,
            _ => return None,
        })
    }

    fn run(self, base_dir: &Path) -> Result<(), String> {
        match self {
            Self::SetHosts => hosts::set_redirects(&REDIRECT_HOSTS),
            Self::ClearHosts => hosts::clear(),
            Self::InstallCa => ca::install(base_dir),
            Self::UninstallCa => ca::uninstall(base_dir),
        }
    }
}

/// hidden CLI entry point: `hebnix-app --priv-action <action> <base_dir>`.
/// Called at the very top of main(), before logging/GUI setup - a plain,
/// no-GUI, near-instant helper invocation, always run via `pkexec` from
/// `run_privileged` below. Returns the process exit code to use if this
/// process was invoked this way, or None if it's a normal launch.
pub const PRIV_ARG: &str = "--priv-action";

pub fn maybe_handle_privileged_cli() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(PRIV_ARG) {
        return None;
    }
    let action = args.next().as_deref().and_then(PrivilegedAction::from_str);
    let base_dir = args.next().map(PathBuf::from);
    let (Some(action), Some(base_dir)) = (action, base_dir) else {
        eprintln!("usage: hebnix-app {PRIV_ARG} <action> <base_dir>");
        return Some(1);
    };
    match action.run(&base_dir) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("{error}");
            Some(1)
        }
    }
}

/// runs one of the two privileged actions, elevating via a one-shot
/// `pkexec` call if not already root. Blocks until it's done (including
/// however long a real polkit auth prompt takes) - unlike the old
/// whole-app relaunch this replaced, this never exits or replaces the
/// caller, it just waits and returns, since it's a single quick action
/// rather than "become the new long-running process."
pub fn run_privileged(action: PrivilegedAction, base_dir: &Path) -> Result<(), String> {
    if is_admin() {
        return action.run(base_dir);
    }
    let Ok(exe) = std::env::current_exe() else {
        return Err("could not find our own executable path".to_string());
    };
    tracing::info!("spoofer: running privileged action {:?} via pkexec", action.as_str());
    let status = std::process::Command::new("pkexec")
        .arg(exe)
        .arg(PRIV_ARG)
        .arg(action.as_str())
        .arg(base_dir)
        .status()
        .map_err(|error| format!("could not run pkexec: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "elevated {} failed or was declined",
            action.as_str()
        ))
    }
}

fn marker_path(base_dir: &Path) -> PathBuf {
    ca::dir(base_dir).join("proxy_backup.json")
}

// linux-port: dropped the windows-only system-proxy (Internet Settings /
// InternetSetOptionW) backup+restore path entirely. Rocket League on Linux
// runs under Proton/Steam Play and doesn't consult the host desktop's HTTP
// proxy settings the way native Windows apps do via WinInet, so there's
// nothing analogous to back up or restore here -- the mitm proxy is reached
// purely through the /etc/hosts redirect (see hosts.rs).

pub fn restore_if_crashed(base_dir: &Path) {
    // stale marker from a pre-linux-port crash; nothing to restore anymore,
    // just clean it up.
    let _ = std::fs::remove_file(marker_path(base_dir));
    if hosts::has_redirects() {
        tracing::warn!("stale hosts redirect, clearing it");
        // best-effort: this may prompt for auth right at startup for a
        // redirect left over from an unclean exit. Leaving a stale MITM
        // redirect in place silently is worse, so this is deliberately not
        // swallowed the way it briefly was - if declined, it just tries
        // again next launch.
        if let Err(error) = run_privileged(PrivilegedAction::ClearHosts, base_dir) {
            tracing::warn!("spoofer: couldn't clear the stale hosts redirect: {error}");
        } else {
            hosts::flush_dns();
        }
    }
}

pub struct SpooferManager {
    base_dir: PathBuf,
    tx: Sender<AppMsg>,
    spoofed_name: Arc<Mutex<String>>,
    pub spoofed_friends: Arc<Mutex<HashMap<String, String>>>,
    pub discovered_friends: Arc<Mutex<HashMap<String, String>>>,
    pub spoofed_ranks: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
    owned_products: Arc<Mutex<HashSet<i64>>>,
    reverse_proxy: Mutex<Option<SocketProxy>>,
    http_active: Arc<AtomicBool>,
    socket_active: Arc<AtomicBool>,
    title_settings: Arc<Mutex<TitleSettings>>,
    skill_bridge: Mutex<Option<SkillBridge>>,
    item_spawner_enabled: Arc<AtomicBool>,
    spawned_items: crate::item_spawning::SpawnedItemLedger,
    crl: Mutex<Option<crl::CrlServer>>,
    // start_http() and start_socket() each independently call
    // ensure_reverse_proxy() - if the first one's pkexec call fails (auth
    // declined/slow/whatever), the second one used to immediately retry
    // the whole thing again from scratch, including its own fresh pkexec
    // prompt, purely because it has no memory of the first one just
    // failing. Confirmed live: enabling both proxies at once (the normal
    // startup case with saved settings) fired two separate set-hosts
    // prompts a fraction of a second apart. Caching a recent failure here
    // for a few seconds means the second caller in the same burst gets the
    // same error back instead of trying pkexec all over again.
    recent_reverse_proxy_failure: Mutex<Option<(std::time::Instant, String)>>,
    rlapi_retained: AtomicBool,
}

impl SpooferManager {
    fn ensure_crl(&self, ca: &ca::Ca) {
        let mut slot = match self.crl.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if slot.is_some() {
            return;
        }
        let der = match ca.crl_der() {
            Ok(d) => d,
            Err(e) => {
                let _ = self.tx.send(AppMsg::Log(format!("[Spoofer] crl gen: {e}")));
                return;
            }
        };
        match crl::CrlServer::start(der, self.tx.clone()) {
            Ok(s) => *slot = Some(s),
            Err(e) => {
                let _ = self
                    .tx
                    .send(AppMsg::Log(format!("[Spoofer] crl server: {e}")));
            }
        }
    }

    fn maybe_stop_crl(&self) {
        let proxy_up = self
            .reverse_proxy
            .lock()
            .map(|p| p.is_some())
            .unwrap_or(false);
        if proxy_up {
            return;
        }
        if let Ok(mut slot) = self.crl.lock() {
            if let Some(s) = slot.take() {
                s.stop();
            }
        }
    }

    pub fn new(base_dir: PathBuf, tx: Sender<AppMsg>) -> Self {
        let owned_products = std::fs::read(base_dir.join("owned_products.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<i64>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        let spawned_items = crate::item_spawning::SpawnedItemLedger::new(&base_dir);
        Self {
            base_dir,
            tx,
            spoofed_name: Arc::new(Mutex::new(String::new())),
            spoofed_friends: Arc::new(Mutex::new(HashMap::new())),
            discovered_friends: Arc::new(Mutex::new(HashMap::new())),
            spoofed_ranks: Arc::new(Mutex::new(HashMap::new())),
            owned_products: Arc::new(Mutex::new(owned_products)),
            reverse_proxy: Mutex::new(None),
            http_active: Arc::new(AtomicBool::new(false)),
            socket_active: Arc::new(AtomicBool::new(false)),
            title_settings: Arc::new(Mutex::new(TitleSettings::default())),
            skill_bridge: Mutex::new(None),
            item_spawner_enabled: Arc::new(AtomicBool::new(false)),
            spawned_items,
            crl: Mutex::new(None),
            recent_reverse_proxy_failure: Mutex::new(None),
            rlapi_retained: AtomicBool::new(false),
        }
    }

    pub fn rlapi_running(&self) -> bool {
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        session.enabled()
            || session.has_connection()
            || (self.rlapi_retained.load(Ordering::Acquire)
                && hebnix_sdk::process::is_rocket_league_running())
    }

    fn redirect_hosts(&self) -> &'static [&'static str] {
        if self.http_active.load(Ordering::Relaxed) || self.socket_active.load(Ordering::Relaxed) {
            &REDIRECT_HOSTS
        } else {
            &[TITLE_HOST]
        }
    }

    pub fn enable_rlapi(&self) -> Result<(), String> {
        let can_bind = is_admin() || crate::multiplayer_lan::has_net_bind_service_capability();
        if !can_bind || !ca::is_current_installed(&self.base_dir) {
            return Err("RLAPI capture requires the port 443 permission and the existing Hebnix certificate setup.".into());
        }
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        session.set_enabled(true);
        let result = self
            .start_skill_bridge()
            .and_then(|_| self.ensure_reverse_proxy());
        if let Err(error) = result {
            session.set_enabled(false);
            self.stop_skill_bridge();
            self.stop_reverse_if_unused();
            return Err(error);
        }
        self.rlapi_retained.store(true, Ordering::Release);
        Ok(())
    }

    pub fn disable_rlapi(&self) {
        hebnix_sdk::rlapi::session::shared_game_session().set_enabled(false);
        // A connected game retains its relay until it disconnects. Disabling
        // API requests must not kick the game out of its current session.
        self.cleanup_idle_rlapi();
    }

    pub fn cleanup_idle_rlapi(&self) {
        if !self.rlapi_running() {
            self.stop_reverse_if_unused();
            if !self.http_active.load(Ordering::Relaxed)
                && !self.socket_active.load(Ordering::Relaxed)
            {
                self.stop_skill_bridge();
            }
            self.maybe_stop_crl();
            // Cached localhost endpoints must not survive the final relay.
            // Retry on later monitor ticks if Windows still holds cache files.
            if self.rlapi_retained.load(Ordering::Acquire)
                && !hebnix_sdk::process::is_rocket_league_running()
                && crate::winutil::clear_rocket_league_web_cache().is_ok()
            {
                self.rlapi_retained.store(false, Ordering::Release);
            }
        }
    }
    pub fn owned_product_ids(&self) -> HashSet<i64> {
        self.owned_products
            .lock()
            .map(|owned| owned.clone())
            .unwrap_or_default()
    }

    pub fn set_username(&self, name: &str) {
        if let Ok(mut guard) = self.spoofed_name.lock() {
            let truncated: String = name.chars().take(MAX_NAME_LENGTH).collect();
            *guard = truncated;
        }
    }

    pub fn update_friends(&self, spoofs: HashMap<String, String>) {
        if let Ok(mut guard) = self.spoofed_friends.lock() {
            *guard = spoofs;
        }
    }

    pub fn update_ranks(&self, ranks: HashMap<i32, (i32, f64)>) {
        if let Ok(mut guard) = self.spoofed_ranks.lock() {
            *guard = ranks;
        }
        if self
            .spoofed_ranks
            .lock()
            .map(|ranks| !ranks.is_empty())
            .unwrap_or(false)
        {
            if let Err(error) = self.start_skill_bridge() {
                let detail = format!("[Spoofer] Rank bridge failed to start: {error}");
                let _ = std::fs::write(self.base_dir.join("rank_spoofer_status.log"), &detail);
                let _ = self.tx.send(AppMsg::Log(detail));
            }
        } else if !self.item_spawner_enabled.load(Ordering::Relaxed) {
            self.stop_skill_bridge();
        }
    }

    pub fn set_item_spawner_enabled(&self, enabled: bool) -> Result<(), String> {
        if enabled {
            self.start_skill_bridge()?;
            self.item_spawner_enabled.store(true, Ordering::SeqCst);
            Ok(())
        } else {
            self.item_spawner_enabled.store(false, Ordering::SeqCst);
            let ranks_active = self
                .spoofed_ranks
                .lock()
                .map(|ranks| !ranks.is_empty())
                .unwrap_or(false);
            if !ranks_active {
                self.stop_skill_bridge();
            }
            Ok(())
        }
    }

    fn start_skill_bridge(&self) -> Result<(), String> {
        let mut slot = self
            .skill_bridge
            .lock()
            .map_err(|_| "rank bridge lock poisoned")?;
        if slot.is_none() {
            *slot = Some(SkillBridge::start(
                Arc::clone(&self.spoofed_ranks),
                self.tx.clone(),
                self.base_dir.join("rank_spoofer_frames.log"),
                &self.base_dir,
            )?);
        }
        Ok(())
    }

    fn stop_skill_bridge(&self) {
        if self.rlapi_running() {
            return;
        }
        if let Ok(mut slot) = self.skill_bridge.lock() {
            if let Some(bridge) = slot.take() {
                bridge.stop();
            }
        }
    }

    pub fn http_running(&self) -> bool {
        self.http_active.load(Ordering::Relaxed)
            && self
                .reverse_proxy
                .lock()
                .map(|proxy| proxy.is_some())
                .unwrap_or(false)
    }

    pub fn socket_running(&self) -> bool {
        self.socket_active.load(Ordering::Relaxed)
            && self
                .reverse_proxy
                .lock()
                .map(|proxy| proxy.is_some())
                .unwrap_or(false)
    }

    pub fn start_http(&self) -> Result<(), String> {
        if self.http_active.load(Ordering::Relaxed) {
            return self.reconcile_hosts();
        }
        self.http_active.store(true, Ordering::Relaxed);
        if let Err(error) = self.ensure_reverse_proxy() {
            self.http_active.store(false, Ordering::Relaxed);
            return Err(error);
        }
        if let Err(error) = self.start_skill_bridge() {
            self.http_active.store(false, Ordering::Relaxed);
            self.stop_reverse_if_unused();
            return Err(error);
        }
        Ok(())
    }

    pub fn stop_http(&self) {
        self.http_active.store(false, Ordering::Relaxed);
        if !self.socket_active.load(Ordering::Relaxed) {
            self.stop_skill_bridge();
        }
        self.stop_reverse_if_unused();
        self.maybe_stop_crl();
    }

    pub fn set_titles(&self, titles: Vec<TitleSpoofSettings>) {
        if let Ok(mut settings) = self.title_settings.lock() {
            let mut normalized: Vec<TitleSpoofSettings> = titles
                .into_iter()
                .filter_map(|mut title| {
                    title.text = title.text.trim().chars().take(64).collect();
                    (!title.text.is_empty()).then_some(title)
                })
                .collect();
            let mut seen = HashSet::new();
            normalized.retain(|title| seen.insert(title.target_id.clone()));
            settings.titles = normalized;
        }
    }

    pub fn set_title_enabled(&self, enabled: bool) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.enabled = enabled;
        }
    }

    pub fn start_socket(&self) -> Result<(), String> {
        if self.socket_active.load(Ordering::Relaxed) {
            return self.reconcile_hosts();
        }
        self.socket_active.store(true, Ordering::Relaxed);
        if let Err(error) = self.ensure_reverse_proxy() {
            self.socket_active.store(false, Ordering::Relaxed);
            return Err(error);
        }
        if let Err(error) = self.start_skill_bridge() {
            self.socket_active.store(false, Ordering::Relaxed);
            self.stop_reverse_if_unused();
            return Err(error);
        }
        Ok(())
    }

    pub fn spawn_item(
        &self,
        request: &crate::item_spawning::ItemSpawnRequest,
    ) -> Result<(), String> {
        if !self.item_spawner_enabled.load(Ordering::Relaxed) {
            return Err("Enable Item Spawning first".into());
        }
        if !self.item_spawner_websocket_connected() {
            return Err("Wait for Rocket League's PsyNet WebSocket to connect".into());
        }

        self.start_skill_bridge()?;
        let psy_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs() as i64;
        let (message, instance_ids) = crate::item_spawning::reward_message(request, psy_time)?;
        self.spawned_items
            .record(&instance_ids)
            .map_err(|error| format!("Could not track spawned item: {error}"))?;
        let slot = self
            .skill_bridge
            .lock()
            .map_err(|_| "item bridge lock poisoned")?;
        slot.as_ref()
            .ok_or_else(|| "PsyNet websocket bridge is not running".to_string())?
            .send_text(message)
    }

    pub fn item_spawner_websocket_connected(&self) -> bool {
        self.skill_bridge
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(SkillBridge::is_connected))
            .unwrap_or(false)
    }

    pub fn stop_socket(&self) {
        self.socket_active.store(false, Ordering::Relaxed);
        self.stop_reverse_if_unused();
        self.stop_skill_bridge();
        self.maybe_stop_crl();
    }

    fn ensure_reverse_proxy(&self) -> Result<(), String> {
        const RETRY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);
        if let Ok(guard) = self.recent_reverse_proxy_failure.lock() {
            if let Some((at, error)) = guard.as_ref() {
                if at.elapsed() < RETRY_COOLDOWN {
                    return Err(error.clone());
                }
            }
        }
        let result = self.ensure_reverse_proxy_uncached();
        if let Ok(mut guard) = self.recent_reverse_proxy_failure.lock() {
            *guard = result.as_ref().err().map(|error| (std::time::Instant::now(), error.clone()));
        }
        result
    }

    fn ensure_reverse_proxy_uncached(&self) -> Result<(), String> {
        let mut slot = self
            .reverse_proxy
            .lock()
            .map_err(|_| "reverse proxy lock poisoned")?;
        if slot.is_some() {
            return hosts::set_redirects(self.redirect_hosts());
        }
        let ca = Arc::new(ca::ensure(&self.base_dir)?);
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "Certificate not installed. open Spoofer settings and click Install Certificate"
                    .into(),
            );
        }
        // the local MITM proxy binds 127.0.0.1:443 directly (a privileged
        // port) - needs CAP_NET_BIND_SERVICE, granted the same way as
        // Workshop LAN's CAP_NET_ADMIN (one setcap covers both). Checked
        // up front for a clear message instead of a raw OS bind error.
        if !crate::multiplayer_lan::has_net_bind_service_capability() && !is_admin() {
            return Err(
                "Needs one extra permission to bind port 443 - open Spoofer settings and click \
                 Grant Permission"
                    .into(),
            );
        }
        let mut real_ips = HashMap::new();
        for host in REDIRECT_HOSTS {
            real_ips.insert(host.to_string(), dns::resolve_a(host)?);
        }
        real_ips.insert(
            "api.rlpp.psynet.gg".to_string(),
            dns::resolve_a("api.rlpp.psynet.gg")?,
        );
        let spoof_rules: Vec<Box<dyn Rule>> = vec![
            Box::new(NameRule::new(Arc::clone(&self.spoofed_name))),
            Box::new(crate::spoofer::rules::FriendsRule::new(
                Arc::clone(&self.spoofed_friends),
                Arc::clone(&self.discovered_friends),
            )),
            Box::new(OwnedProductsRule::new(
                Arc::clone(&self.owned_products),
                self.base_dir.join("owned_products.json"),
            )),
            Box::new(TitleRule::new(Arc::clone(&self.title_settings))),
            Box::new(crate::spoofer::rules::RankRule::with_item_spawner(
                Arc::clone(&self.spoofed_ranks),
                Arc::clone(&self.item_spawner_enabled),
            )),
        ];
        let mut rules: Vec<Box<dyn Rule>> = spoof_rules
            .into_iter()
            .map(|rule| {
                Box::new(crate::spoofer::rules::EnabledRule {
                    inner: rule,
                    http: Arc::clone(&self.http_active),
                    socket: Arc::clone(&self.socket_active),
                }) as Box<dyn Rule>
            })
            .collect();
        rules.push(Box::new(crate::spoofer::rules::RlApiRouteRule));
        let rules = Arc::new(rules);
        self.ensure_crl(&ca);
        let proxy = SocketProxy::start(ca, rules, self.tx.clone(), real_ips)?;
        // one-shot pkexec, blocking (see run_privileged's doc) - matches
        // the existing "Install Certificate" button, which was already a
        // synchronous, potentially slow/prompting UI action even before
        // this refactor.
        if let Err(error) = run_privileged(PrivilegedAction::SetHosts, &self.base_dir) {
            proxy.stop();
            return Err(error);
        }
        hosts::flush_dns();
        *slot = Some(proxy);
        Ok(())
    }

    /// Reconcile hosts with the shared proxy's actual runtime state.
    pub fn reconcile_hosts(&self) -> Result<(), String> {
        if self.http_running() || self.socket_running() || self.rlapi_running() {
            hosts::set_redirects(self.redirect_hosts())
        } else {
            hosts::clear()
        }
    }

    fn stop_reverse_if_unused(&self) {
        if self.http_active.load(Ordering::Relaxed)
            || self.socket_active.load(Ordering::Relaxed)
            || self.rlapi_running()
        {
            return;
        }
        // stop_socket() and stop_http() both call this - only actually the
        // pkexec round trip once (has_redirects() is a plain file read, no
        // root needed), or toggling one proxy off with the other already
        // off used to prompt for authentication twice in a row for nothing.
        if hosts::has_redirects() {
            let _ = run_privileged(PrivilegedAction::ClearHosts, &self.base_dir);
            hosts::flush_dns();
        }
        if let Ok(mut slot) = self.reverse_proxy.lock() {
            if let Some(proxy) = slot.take() {
                proxy.stop();
            }
        }
    }

    /// Stops only runtime interception. It deliberately does not modify saved
    /// spoof settings, so the user's enabled toggles survive the next launch.
    pub fn shutdown(&self) {
        self.rlapi_retained.store(false, Ordering::Release);
        hebnix_sdk::rlapi::session::shared_game_session().reset();
        self.item_spawner_enabled.store(false, Ordering::SeqCst);
        self.stop_socket();
        self.stop_http();
        // Clear a redirect even if the socket failed to start or its state
        // was lost - but stop_socket()/stop_http() above already did this
        // via stop_reverse_if_unused() in the normal case, so skip the
        // redundant (and potentially auth-prompting) third call. Only flush
        // DNS if we actually just cleared something here - `resolvectl
        // flush-caches` needs its own separate polkit authorization on some
        // setups, so calling it unconditionally on every close was itself a
        // second auth prompt even when spoofer was never enabled this run.
        if hosts::has_redirects() {
            let _ = run_privileged(PrivilegedAction::ClearHosts, &self.base_dir);
            hosts::flush_dns();
        }
    }
}
