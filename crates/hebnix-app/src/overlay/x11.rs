//! X11 overlay: a click-through ARGB override-redirect window over the
//! monitor Rocket League is on, painted with tiny-skia like the Wayland one.
//!
//! Used on X11 desktops (i3, Linux Mint's Cinnamon, XFCE, MATE, ...), where
//! there is no layer-shell. It needs a compositing manager (Cinnamon's Muffin
//! has one, i3 needs picom or similar): without one an ARGB window draws with
//! an opaque black background, so `new` refuses to start.

use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux,
    EventMask, ImageFormat, PropMode, StackMode, VisualClass, WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// how often the monitor geometry is looked up again
const GEOMETRY_INTERVAL: Duration = Duration::from_millis(750);

pub struct X11Overlay {
    conn: RustConnection,
    root: u32,
    window: u32,
    gc: u32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    mapped: bool,
    closed: bool,
    geometry_checked: Option<Instant>,
}

/// (x, y, width, height) of the monitor RL is on, else the first monitor,
/// else the root window's size
fn target_geometry(root_size: (u32, u32)) -> (i32, i32, u32, u32) {
    let rect = hebnix_sdk::process::rocket_league_monitor_rect()
        .or_else(|| hebnix_sdk::process::x11::monitors().first().copied());
    match rect {
        Some((l, t, r, b)) if r > l && b > t => (l, t, (r - l) as u32, (b - t) as u32),
        _ => (0, 0, root_size.0, root_size.1),
    }
}

/// tiny-skia stores premultiplied RGBA; an ARGB32 X visual wants premultiplied
/// little-endian 0xAARRGGBB, i.e. byte order B,G,R,A
fn rgba_to_bgra(src: &[u8], dst: &mut [u8]) {
    for (dst, src) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = src[3];
    }
}

impl X11Overlay {
    pub fn new() -> Result<Self, String> {
        if !hebnix_sdk::process::x11::compositor_running() {
            return Err("no compositing manager is running, so a transparent overlay isn't \
                        possible (Cinnamon/Muffin has one; i3 needs picom or similar)"
                .to_string());
        }
        let (conn, screen_num) =
            x11rb::connect(None).map_err(|e| format!("X11 connect failed: {e}"))?;
        let screen = conn.setup().roots[screen_num].clone();
        let visual = screen
            .allowed_depths
            .iter()
            .filter(|depth| depth.depth == 32)
            .flat_map(|depth| depth.visuals.iter())
            .find(|visual| visual.class == VisualClass::TRUE_COLOR && visual.red_mask == 0x00ff_0000)
            .map(|visual| visual.visual_id)
            .ok_or("the X server has no 32-bit ARGB visual")?;

        let (x, y, width, height) = target_geometry((
            screen.width_in_pixels as u32,
            screen.height_in_pixels as u32,
        ));

        let colormap = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_colormap(ColormapAlloc::NONE, colormap, screen.root, visual)
            .map_err(|e| e.to_string())?;
        let window = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_window(
            32,
            window,
            screen.root,
            x as i16,
            y as i16,
            width as u16,
            height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(0)
                .border_pixel(0)
                .colormap(colormap)
                .event_mask(EventMask::NO_EVENT),
        )
        .map_err(|e| e.to_string())?;

        // click-through: an empty input shape, so pointer events fall through
        conn.shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            x11rb::protocol::xproto::ClipOrdering::UNSORTED,
            window,
            0,
            0,
            &[],
        )
        .map_err(|e| format!("XShape unavailable: {e}"))?;

        // a recognisable class for compositor rules, and no picom shadow
        let _ = conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"hebnix-overlay\0hebnix-overlay\0",
        );
        let _ = conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"hebnix-overlay",
        );
        if let Ok(cookie) = conn.intern_atom(false, b"_COMPTON_SHADOW") {
            if let Ok(atom) = cookie.reply() {
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    window,
                    atom.atom,
                    AtomEnum::CARDINAL,
                    &[0],
                );
            }
        }

        let gc = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_gc(gc, window, &CreateGCAux::new())
            .map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;

        Ok(Self {
            conn,
            root: screen.root,
            window,
            gc,
            x,
            y,
            width,
            height,
            mapped: false,
            closed: false,
            geometry_checked: Some(Instant::now()),
        })
    }

    /// drain pending events (errors surface here), keep the window on the
    /// right monitor, and return its size. None once the connection died.
    pub fn poll_size(&mut self) -> Option<(u32, u32)> {
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("overlay: X11 connection lost: {e}");
                    self.closed = true;
                    return None;
                }
            }
        }
        let due = self
            .geometry_checked
            .map(|at| at.elapsed() >= GEOMETRY_INTERVAL)
            .unwrap_or(true);
        if due {
            self.geometry_checked = Some(Instant::now());
            let root_size = self
                .conn
                .get_geometry(self.root)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|geometry| (geometry.width as u32, geometry.height as u32))
                .unwrap_or((self.width, self.height));
            let (x, y, width, height) = target_geometry(root_size);
            if (x, y, width, height) != (self.x, self.y, self.width, self.height) {
                let _ = self.conn.configure_window(
                    self.window,
                    &ConfigureWindowAux::new()
                        .x(x)
                        .y(y)
                        .width(width)
                        .height(height),
                );
                (self.x, self.y, self.width, self.height) = (x, y, width, height);
            }
        }
        (self.width > 0 && self.height > 0).then_some((self.width, self.height))
    }

    /// paint `pixmap` (must match the current size) and show the window
    pub fn present(&mut self, pixmap: &tiny_skia::Pixmap) {
        let (width, height) = (self.width, self.height);
        if pixmap.width() != width || pixmap.height() != height || width == 0 || height == 0 {
            return;
        }
        let mut data = vec![0u8; pixmap.data().len()];
        rgba_to_bgra(pixmap.data(), &mut data);

        // put_image requests are limited by the server's maximum request size
        let row_bytes = width as usize * 4;
        let max_rows = (self.conn.maximum_request_bytes().saturating_sub(64) / row_bytes).max(1);
        let mut y = 0usize;
        while y < height as usize {
            let rows = max_rows.min(height as usize - y);
            let chunk = &data[y * row_bytes..(y + rows) * row_bytes];
            if let Err(e) = self.conn.put_image(
                ImageFormat::Z_PIXMAP,
                self.window,
                self.gc,
                width as u16,
                rows as u16,
                0,
                y as i16,
                0,
                32,
                chunk,
            ) {
                tracing::warn!("overlay: put_image failed: {e}");
                self.closed = true;
                return;
            }
            y += rows;
        }

        if !self.mapped {
            let _ = self.conn.map_window(self.window);
            self.mapped = true;
        }
        // stay above the game even if something else was raised since
        let _ = self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        if self.conn.flush().is_err() {
            self.closed = true;
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn hide(&mut self) {
        if !self.mapped {
            return;
        }
        let _ = self.conn.unmap_window(self.window);
        self.mapped = false;
        if self.conn.flush().is_err() {
            self.closed = true;
        }
    }
}

impl Drop for X11Overlay {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_red_and_blue_keeping_alpha() {
        let src = [10u8, 20, 30, 40, 1, 2, 3, 4];
        let mut dst = [0u8; 8];
        rgba_to_bgra(&src, &mut dst);
        assert_eq!(dst, [30, 20, 10, 40, 3, 2, 1, 4]);
    }
}
