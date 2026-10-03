mod app;
mod patcher;
mod ball {
    pub use crate::patcher::ball::*;
}
mod boost_patcher {
    pub use crate::patcher::boost_patcher::*;
}
mod colours {
    pub use crate::patcher::colours::*;
}
mod car_patcher {
    pub use crate::patcher::car_patcher::*;
}
mod config;
mod rl_launch;
mod cosmetic_thumbnail {
    pub use crate::patcher::cosmetic_thumbnail::*;
}
mod cosmetic_upk {
    pub use crate::patcher::cosmetic_upk::*;
}
mod decal_patcher {
    pub use crate::patcher::decal_patcher::*;
}
mod deep_link;
mod discord_presence;
mod dpi_fix;
mod hotkey;
mod i18n;
#[path = "item_spawning/mod.rs"]
mod item_spawning;
mod messages;
mod monitor;
mod multiplayer_lan;
mod overlay;
mod veryimportantfile;
mod patch_core {
    pub use crate::patcher::patch_core::*;
}
mod plugins;
mod presets;
mod spoofer;
mod statsapi_ini;
mod speed_patch {
    pub use crate::patcher::speed_patch::*;
}
mod swapper {
    pub use crate::patcher::swapper::*;
}
mod theme;
mod tray;
mod ui;
mod upk_keys {
    pub use crate::patcher::upk_keys::*;
}
mod watchdog;
mod webview;
mod winutil;

use app::HebnixApp;

fn load_window_icon(base_dir: &std::path::Path) -> Option<eframe::egui::IconData> {
    let bytes =
        std::fs::read(base_dir.join("hebnix.ico")).unwrap_or_else(|_| tray::EMBEDDED_ICON.to_vec());
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(eframe::egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

fn open_fresh_log(base_dir: &std::path::Path) -> std::io::Result<std::fs::File> {
    let log_path = base_dir.join("hebnix.log");
    let old_log_path = base_dir.join("hebnix.log.old");

    match std::fs::remove_file(&old_log_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match std::fs::rename(&log_path, &old_log_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)
}

fn setup_logging(base_dir: &std::path::Path) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into());

    // sync writer, nothing buffered so the log survives a hard crash.
    // volume's low enough that sync writes don't matter.
    let log_file = open_fresh_log(base_dir).map_err(|error| {
        eprintln!("failed to refresh hebnix.log: {error}");
    });

    match log_file {
        Ok(file) => {
            let file = std::sync::Mutex::new(file);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(file.and(std::io::stdout))
                .init();
        }
        Err(()) => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        }
    }
}

/// On panic dump msg + backtrace to AppData's crash.txt, also log it,
/// then fall through to the default hook.
fn setup_panic_hook(base_dir: &std::path::Path) {
    let crash_path = base_dir.join("crash.txt");
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = spoofer::hosts::clear();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let thread = std::thread::current();
        let text = format!(
            "=== PANIC (unix time {ts}, thread {:?}) ===\n{info}\n\nbacktrace:\n{backtrace}\n\n",
            thread.name().unwrap_or("<unnamed>")
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_path)
        {
            use std::io::Write;
            let _ = f.write_all(text.as_bytes());
        }
        tracing::error!("PANIC: {info}");
        default_hook(info);
    }));
}

fn main() -> eframe::Result {
    // WebKitGTK's own escape hatch for a driver bug in its GPU-accelerated
    // compositing path (DMA-BUF + explicit sync via
    // wp_linux_drm_syncobj_surface_v1) -- confirmed live to throw a fatal
    // Wayland protocol error ("Missing acquire timeline") that GTK treats
    // as unrecoverable and aborts the whole process on, at least on this
    // NVIDIA/Wayland combo. The html plugin overlay (webview.rs) is plain
    // CSS/DOM, nothing worth GPU-compositing, so this is a pure safety net.
    // Must be set before any other thread exists (env vars aren't safely
    // mutable once threads that might read the environment are running),
    // so first thing in main(), before anything else spawns.
    // SAFETY: single-threaded here, nothing else has run yet.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        // GTK3 defaults to the X11 backend (via XWayland, which is present
        // here -- DISPLAY is set) unless told otherwise, even under a
        // native Wayland session. gtk-layer-shell requires a real Wayland
        // GdkWindow to do anything -- on an X11-backed window
        // init_layer_shell() silently no-ops, so both the tray icon and the
        // html overlay's window need this forced.
        std::env::set_var("GDK_BACKEND", "wayland");
    }

    // hidden one-shot helper mode (`--priv-action <action> <base_dir>`),
    // always invoked via pkexec by spoofer::run_privileged for just the two
    // things this app ever needs root for - see PrivilegedAction's doc.
    // Checked before any of the normal startup (base_dir/logging/single-
    // instance/GUI) since it's a completely separate, near-instant,
    // no-window invocation.
    if let Some(code) = spoofer::maybe_handle_privileged_cli() {
        std::process::exit(code);
    }

    let base_dir = config::base_dir();
    if let Some(parent_pid) = watchdog::parent_pid() {
        watchdog::run(parent_pid);
        return Ok(());
    }
    setup_logging(&base_dir);
    deep_link::register_and_queue_from_args(&base_dir);
    setup_panic_hook(&base_dir);
    tracing::info!("Hebnix {} starting", app::APP_VERSION);
    // no-op unless the binary was setcap'd (see multiplayer_lan::GRANTED_CAPS) -
    // lets Workshop LAN multiplayer's `nft`/`tailscaled` child processes
    // inherit the capabilities instead of needing them set on those binaries.
    multiplayer_lan::raise_net_admin_ambient();

    let cfg = config::Config::load(&base_dir);
    i18n::init(&cfg.settings.language, Some(base_dir.join("locales")));

    // single instance guard
    let Some(_lock) = winutil::acquire_single_instance() else {
        winutil::focus_existing_instance();
        return Ok(());
    };

    spoofer::restore_if_crashed(&base_dir);
    let _ = watchdog::spawn();

    #[cfg(target_os = "linux")]
    hebnix_sdk::process::exempt_own_window_decorations();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Hebnix")
        .with_inner_size([
            (cfg.window.width as f32).max(app::MIN_WIDTH),
            (cfg.window.height as f32).max(app::MIN_HEIGHT),
        ])
        .with_min_inner_size([app::MIN_WIDTH, app::MIN_HEIGHT])
        .with_transparent(true)
        .with_decorations(false)
        .with_visible(!cfg.settings.start_in_tray);
    // only actually honored on X11 - Wayland's xdg_toplevel has no "set
    // position" request on any compositor, by protocol design (see
    // config::WindowCfg's x/y doc comment)
    if let (Some(x), Some(y)) = (cfg.window.x, cfg.window.y) {
        viewport = viewport.with_position([x as f32, y as f32]);
    }
    if let Some(icon) = load_window_icon(&base_dir) {
        viewport = viewport.with_icon(icon);
    }

    // linux-port: the windows build forces a DX12 wgpu backend bound to a
    // DirectComposition visual (dcomp_wgpu_options in the reference). On
    // Wayland the default eframe/wgpu backend selection (Vulkan via
    // WGPU_BACKEND, falling back to GL through the "glow" feature already
    // enabled in Cargo.toml) is the right call -- no analogous backend
    // pinning needed.
    let options = eframe::NativeOptions {
        viewport,
        // linux-port: plugin windows render as egui "immediate viewports" -
        // synchronous nested paints called mid-frame, inside the outer
        // winit/wayland event dispatch. Their SwapBuffers call pumps the
        // same wl_display connection the outer dispatch is already
        // pumping; with vsync on, NVIDIA's EGL-on-Wayland layer blocks
        // that nested SwapBuffers waiting on a frame callback only the
        // (currently-blocked) outer dispatch could ever deliver, freezing
        // the whole app the moment any plugin window is visible. Disabling
        // vsync makes SwapBuffers return immediately instead of waiting on
        // the compositor, breaking that deadlock.
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let result = eframe::run_native(
        "Hebnix",
        options,
        Box::new(|cc| Ok(Box::new(HebnixApp::new(cc)))),
    );
    watchdog::finish_live_handoff();
    result
}
