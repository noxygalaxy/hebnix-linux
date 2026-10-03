//! X11 window queries and actions through EWMH, for i3 and the stacking
//! desktops that run on X11 (Linux Mint's Cinnamon, XFCE, MATE, GNOME/KDE on
//! X11). Everything is best-effort and returns `None` / `false` on any X
//! error, or when there is no EWMH window manager (e.g. a bare XWayland).
//!
//! A fresh connection is opened per call: calls come from a few threads a few
//! times a second at most, and it avoids sharing a socket or reconnect logic.

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Window,
};
use x11rb::rust_connection::RustConnection;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_CLIENT_LIST,
        _NET_ACTIVE_WINDOW,
        _NET_WM_PID,
        _NET_WM_NAME,
        _NET_WM_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_CM_S0,
        UTF8_STRING,
    }
}

/// a top-level client window as the window manager lists it
#[derive(Debug, Clone, PartialEq)]
pub struct X11Window {
    pub id: u32,
    pub pid: Option<u32>,
    pub title: String,
    /// `WM_CLASS` instance and class, space separated, e.g. "steam_app_252950 steam_app_252950"
    pub class: String,
    /// (left, top, right, bottom) in root coordinates
    pub rect: (i32, i32, i32, i32),
    pub focused: bool,
    pub desktop: Option<u32>,
}

struct Session {
    conn: RustConnection,
    root: Window,
    atoms: Atoms,
}

impl Session {
    fn open() -> Option<Self> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let atoms = Atoms::new(&conn).ok()?.reply().ok()?;
        Some(Self { conn, root, atoms })
    }

    fn u32s(&self, window: Window, property: u32, kind: impl Into<u32>) -> Option<Vec<u32>> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, 4096)
            .ok()?
            .reply()
            .ok()?;
        reply.value32().map(|values| values.collect())
    }

    fn bytes(&self, window: Window, property: u32, kind: impl Into<u32>) -> Option<Vec<u8>> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        (!reply.value.is_empty()).then_some(reply.value)
    }

    fn title(&self, window: Window) -> String {
        self.bytes(window, self.atoms._NET_WM_NAME, self.atoms.UTF8_STRING)
            .or_else(|| self.bytes(window, AtomEnum::WM_NAME.into(), AtomEnum::STRING))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    }

    fn class(&self, window: Window) -> String {
        self.bytes(window, AtomEnum::WM_CLASS.into(), AtomEnum::STRING)
            .map(|bytes| {
                bytes
                    .split(|byte| *byte == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    }

    fn rect(&self, window: Window) -> Option<(i32, i32, i32, i32)> {
        let geometry = self.conn.get_geometry(window).ok()?.reply().ok()?;
        let origin = self
            .conn
            .translate_coordinates(window, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        let (x, y) = (origin.dst_x as i32, origin.dst_y as i32);
        Some((x, y, x + geometry.width as i32, y + geometry.height as i32))
    }

    fn active(&self) -> Option<Window> {
        self.u32s(self.root, self.atoms._NET_ACTIVE_WINDOW, AtomEnum::WINDOW)?
            .first()
            .copied()
            .filter(|id| *id != 0)
    }

    fn describe(&self, id: Window, active: Option<Window>) -> Option<X11Window> {
        Some(X11Window {
            id,
            pid: self
                .u32s(id, self.atoms._NET_WM_PID, AtomEnum::CARDINAL)
                .and_then(|values| values.first().copied()),
            title: self.title(id),
            class: self.class(id),
            rect: self.rect(id)?,
            focused: active == Some(id),
            desktop: self
                .u32s(id, self.atoms._NET_WM_DESKTOP, AtomEnum::CARDINAL)
                .and_then(|values| values.first().copied()),
        })
    }

    fn send(&self, window: Window, message: u32, data: [u32; 5]) -> bool {
        let event = ClientMessageEvent::new(32, window, message, data);
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        self.conn.send_event(false, self.root, mask, event).is_ok() && self.conn.flush().is_ok()
    }
}

/// every client window the window manager lists. None without an EWMH
/// window manager.
pub fn windows() -> Option<Vec<X11Window>> {
    let session = Session::open()?;
    let ids = session.u32s(session.root, session.atoms._NET_CLIENT_LIST, AtomEnum::WINDOW)?;
    let active = session.active();
    Some(
        ids.into_iter()
            .filter_map(|id| session.describe(id, active))
            .collect(),
    )
}

/// monitor rectangles (left, top, right, bottom), primary first when known
pub fn monitors() -> Vec<(i32, i32, i32, i32)> {
    let Some(session) = Session::open() else {
        return Vec::new();
    };
    let Some(reply) = session
        .conn
        .randr_get_monitors(session.root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return Vec::new();
    };
    let mut monitors = reply.monitors;
    monitors.sort_by_key(|monitor| !monitor.primary);
    monitors
        .into_iter()
        .map(|m| {
            let (x, y) = (m.x as i32, m.y as i32);
            (x, y, x + m.width as i32, y + m.height as i32)
        })
        .collect()
}

/// is a compositing manager running (owns the `_NET_WM_CM_S0` selection).
/// Without one, ARGB windows draw with a black background instead of
/// transparent, so the overlays stay off.
pub fn compositor_running() -> bool {
    let Some(session) = Session::open() else {
        return false;
    };
    session
        .conn
        .get_selection_owner(session.atoms._NET_WM_CM_S0)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.owner != 0)
}

/// the window of this process: matched by pid, else by exact title
pub fn own_window(own_pid: u32, title: &str) -> Option<u32> {
    let windows = windows()?;
    windows
        .iter()
        .find(|window| window.pid == Some(own_pid))
        .or_else(|| windows.iter().find(|window| window.title == title))
        .map(|window| window.id)
}

/// raise and focus `window`, switching to its desktop (`_NET_ACTIVE_WINDOW`
/// from a "pager", which window managers honour without focus-stealing
/// prevention)
pub fn activate(window: u32) -> bool {
    let Some(session) = Session::open() else {
        return false;
    };
    session.send(window, session.atoms._NET_ACTIVE_WINDOW, [2, 0, 0, 0, 0])
}

/// toggle `_NET_WM_STATE_ABOVE` ("always on top")
pub fn set_above(window: u32, above: bool) -> bool {
    let Some(session) = Session::open() else {
        return false;
    };
    let action = if above { 1 } else { 0 };
    session.send(
        window,
        session.atoms._NET_WM_STATE,
        [action, session.atoms._NET_WM_STATE_ABOVE, 0, 2, 0],
    )
}

/// move `window` to a virtual desktop (`_NET_WM_DESKTOP`)
pub fn move_to_desktop(window: u32, desktop: u32) -> bool {
    let Some(session) = Session::open() else {
        return false;
    };
    session.send(window, session.atoms._NET_WM_DESKTOP, [desktop, 2, 0, 0, 0])
}

/// is the window minimized (`_NET_WM_STATE_HIDDEN`)
pub fn is_hidden(window: u32) -> bool {
    let Some(session) = Session::open() else {
        return false;
    };
    session
        .u32s(window, session.atoms._NET_WM_STATE, AtomEnum::ATOM)
        .is_some_and(|states| states.contains(&session.atoms._NET_WM_STATE_HIDDEN))
}

/// pointer position in root coordinates
pub fn pointer() -> Option<(i32, i32)> {
    let session = Session::open()?;
    let reply = session.conn.query_pointer(session.root).ok()?.reply().ok()?;
    Some((reply.root_x as i32, reply.root_y as i32))
}
