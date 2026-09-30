//! RL process detection + window helpers.

pub mod detector;
pub mod window;
pub mod wine_prefix;

pub use detector::{
    RlPlatform, RlProcessInfo, detect_platform, find_rocket_league, get_save_data_path,
    is_rocket_league_running,
};
pub use wine_prefix::{
    candidate_documents_dirs, documents_dir_for_exe, steam_library_steamapps_dirs, wine_prefixes,
};
pub use window::{
    exempt_own_window_decorations, focus_own_window_over_game, get_rocket_league_window_rect,
    hyprland_layer_visible, is_cursor_inside_rl_window, is_rocket_league_focused,
    reassert_own_window_decorations, rocket_league_hwnd, rocket_league_monitor_size,
    unminimize_own_window,
};
