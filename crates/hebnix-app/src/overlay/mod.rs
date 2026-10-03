//! click-through game overlay: wlr-layer-shell on Wayland (`wayland.rs`), an
//! ARGB window on X11 (`x11.rs`).
//!
//! plugins draw via the free fns here (line, rect, ..) which paint onto a
//! tiny-skia pixmap that's shared with the compositor over wl_shm. all on
//! the main thread.
//!
//! Renders fine over RL in either Borderless Windowed or the game's own
//! real fullscreen (F11) -- confirmed live on Hyprland. An earlier version
//! of this comment claimed exclusive fullscreen couldn't be overlaid at
//! all; that was wrong (or described some other compositor's behavior) and
//! led to a since-reverted "force RL out of fullscreen" workaround here
//! that broke Hyprland's native hide-bars-during-fullscreen behavior for
//! no reason. Layer-shell's `Overlay` layer sits above `top` (where bars
//! like waybar live) specifically so it stays visible regardless of what
//! else the compositor is hiding.

mod wayland;
mod x11;

use std::cell::RefCell;

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// rgba color, straight (non-premultiplied) alpha 0-255
#[derive(Clone, Copy)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    fn to_color(self) -> Color {
        Color::from_rgba8(self.0, self.1, self.2, self.3)
    }
}

thread_local! {
    static CANVAS: RefCell<Option<Pixmap>> = const { RefCell::new(None) };
}

fn with_canvas(f: impl FnOnce(&mut Pixmap)) {
    CANVAS.with(|c| {
        if let Some(pixmap) = c.borrow_mut().as_mut() {
            f(pixmap);
        }
    });
}

fn paint_for(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(color.to_color());
    paint.anti_alias = true;
    paint
}

// Drawing primitives called from the Lua `draw` table. No-ops outside a
// frame (canvas unset, i.e. overlay not configured yet / hidden).

pub fn line(x1: f32, y1: f32, x2: f32, y2: f32, color: Rgba, width: f32) {
    with_canvas(|canvas| {
        let mut pb = PathBuilder::new();
        pb.move_to(x1, y1);
        pb.line_to(x2, y2);
        let Some(path) = pb.finish() else { return };
        let paint = paint_for(color);
        let stroke = Stroke {
            width: width.max(1.0),
            ..Default::default()
        };
        canvas.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    });
}

pub fn rect(x: f32, y: f32, w: f32, h: f32, color: Rgba, width: f32, filled: bool) {
    with_canvas(|canvas| {
        let Some(r) = tiny_skia::Rect::from_xywh(x, y, w, h) else {
            return;
        };
        let paint = paint_for(color);
        if filled {
            canvas.fill_rect(r, &paint, Transform::identity(), None);
        } else {
            let pb = PathBuilder::from_rect(r);
            let stroke = Stroke {
                width: width.max(1.0),
                ..Default::default()
            };
            canvas.stroke_path(&pb, &paint, &stroke, Transform::identity(), None);
        }
    });
}

/// rounded-rectangle outline; `radius` is clamped to half the shorter side
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<tiny_skia::Path> {
    let r = radius.max(0.0).min(w.abs().min(h.abs()) / 2.0);
    let mut pb = PathBuilder::new();
    if r <= 0.0 {
        pb.push_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    // cubic approximation of a quarter circle
    let k = r * 0.552_284_75;
    let (right, bottom) = (x + w, y + h);
    pb.move_to(x + r, y);
    pb.line_to(right - r, y);
    pb.cubic_to(right - r + k, y, right, y + r - k, right, y + r);
    pb.line_to(right, bottom - r);
    pb.cubic_to(right, bottom - r + k, right - r + k, bottom, right - r, bottom);
    pb.line_to(x + r, bottom);
    pb.cubic_to(x + r - k, bottom, x, bottom - r + k, x, bottom - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// linear gradient from `c1` to `c2` across the box at `angle` degrees
/// (0 = left to right), optionally with rounded corners
#[allow(clippy::too_many_arguments)]
pub fn gradient(x: f32, y: f32, w: f32, h: f32, c1: Rgba, c2: Rgba, radius: f32, angle: f32) {
    with_canvas(|canvas| {
        let Some(path) = rounded_rect_path(x, y, w, h, radius) else {
            return;
        };
        let rad = angle.to_radians();
        let (dx, dy) = (rad.cos(), rad.sin());
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let half = (w.abs() * dx.abs() + h.abs() * dy.abs()) / 2.0;
        let Some(shader) = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(cx - dx * half, cy - dy * half),
            tiny_skia::Point::from_xy(cx + dx * half, cy + dy * half),
            vec![
                tiny_skia::GradientStop::new(0.0, c1.to_color()),
                tiny_skia::GradientStop::new(1.0, c2.to_color()),
            ],
            tiny_skia::SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        let mut paint = Paint::default();
        paint.shader = shader;
        paint.anti_alias = true;
        canvas.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    });
}

pub fn circle(x: f32, y: f32, radius: f32, color: Rgba, width: f32, filled: bool) {
    with_canvas(|canvas| {
        let mut pb = PathBuilder::new();
        pb.push_circle(x, y, radius.max(0.1));
        let Some(path) = pb.finish() else { return };
        let paint = paint_for(color);
        if filled {
            canvas.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        } else {
            let stroke = Stroke {
                width: width.max(1.0),
                ..Default::default()
            };
            canvas.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    });
}

/// tiny-skia has no text/glyph rendering built in; glyphs are rasterized
/// with ab_glyph (bundled DejaVu Sans, see assets/DejaVuSans-LICENSE.txt)
/// and blitted into the pixmap as coverage masks tinted by `color`.
fn font() -> &'static ab_glyph::FontArc {
    use std::sync::OnceLock;
    static FONT: OnceLock<ab_glyph::FontArc> = OnceLock::new();
    FONT.get_or_init(|| {
        ab_glyph::FontArc::try_from_slice(include_bytes!("../../assets/DejaVuSans.ttf"))
            .expect("bundled DejaVuSans.ttf is a valid font")
    })
}

/// extra advance added per glyph for synthetic bold (the bundled font has a
/// single weight, so bold is faked by drawing each glyph twice, offset)
fn bold_extra(size: f32, bold: bool) -> f32 {
    if bold { (size * 0.04).max(0.5) } else { 0.0 }
}

/// pixel width of a string in the overlay font, for text layout without a
/// live canvas
pub fn measure_text(s: &str, size: f32, bold: bool) -> f32 {
    use ab_glyph::{Font, ScaleFont};

    let font = font();
    let scale = font.pt_to_px_scale(size).unwrap_or(ab_glyph::PxScale::from(size));
    let scaled = font.as_scaled(scale);
    s.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum::<f32>() + bold_extra(size, bold)
}

/// `font` is accepted for API parity with the Windows overlay; only the
/// bundled font is available here. `clip` is a (width, height) box anchored
/// at the start of the text line; glyph pixels outside it are dropped.
#[allow(clippy::too_many_arguments)]
pub fn text(
    x: f32,
    y: f32,
    s: &str,
    color: Rgba,
    size: f32,
    halign: &str,
    _font: &str,
    bold: bool,
    clip: Option<(f32, f32)>,
) {
    use ab_glyph::{Font, ScaleFont};

    let font = font();
    let scale = font.pt_to_px_scale(size).unwrap_or(ab_glyph::PxScale::from(size));
    let scaled = font.as_scaled(scale);
    let extra = bold_extra(size, bold);

    // pre-measure total advance so center/right alignment can offset the
    // whole line before laying out glyphs.
    let total_advance: f32 =
        s.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum::<f32>() + extra;
    let start_x = match halign {
        "center" => x - total_advance / 2.0,
        "right" => x - total_advance,
        _ => x,
    };
    let baseline_y = y + scaled.ascent();
    let clip_box = clip.map(|(w, h)| (start_x, y, start_x + w, y + h));
    let passes: &[f32] = if bold { &[0.0, extra] } else { &[0.0] };

    with_canvas(|canvas| {
        let mut pen_x = start_x;
        for c in s.chars() {
            let glyph_id = font.glyph_id(c);
            let advance = scaled.h_advance(glyph_id);
            for dx in passes {
                let glyph =
                    glyph_id.with_scale_and_position(scale, ab_glyph::point(pen_x + dx, baseline_y));
                let Some(outlined) = font.outline_glyph(glyph) else {
                    continue;
                };
                let bounds = outlined.px_bounds();
                let ox = bounds.min.x as i32;
                let oy = bounds.min.y as i32;
                outlined.draw(|gx, gy, coverage| {
                    if coverage <= 0.0 {
                        return;
                    }
                    let px = ox + gx as i32;
                    let py = oy + gy as i32;
                    if px < 0 || py < 0 || px as u32 >= canvas.width() || py as u32 >= canvas.height() {
                        return;
                    }
                    if let Some((left, top, right, bottom)) = clip_box {
                        let (fx, fy) = (px as f32, py as f32);
                        if fx < left || fx >= right || fy < top || fy >= bottom {
                            return;
                        }
                    }
                    let a = (color.3 as f32 * coverage.min(1.0)) as u8;
                    if a == 0 {
                        return;
                    }
                    // src-over compositing done directly in premultiplied
                    // space (tiny-skia's native pixel format): out = src +
                    // dst*(1-sa), where dst's channels are already
                    // premultiplied by its own alpha.
                    let idx = (py as u32 * canvas.width() + px as u32) as usize;
                    let dst = &mut canvas.pixels_mut()[idx];
                    let (dr, dg, db, da) = (
                        dst.red() as f32,
                        dst.green() as f32,
                        dst.blue() as f32,
                        dst.alpha() as f32,
                    );
                    let sa = a as f32 / 255.0;
                    let inv_sa = 1.0 - sa;
                    let blend = |s: u8, d: f32| -> u8 {
                        (s as f32 * sa + d * inv_sa).round().clamp(0.0, 255.0) as u8
                    };
                    let out_a = (a as f32 + da * inv_sa).round().clamp(0.0, 255.0) as u8;
                    let Some(px_color) = tiny_skia::PremultipliedColorU8::from_rgba(
                        blend(color.0, dr),
                        blend(color.1, dg),
                        blend(color.2, db),
                        out_a,
                    ) else {
                        return;
                    };
                    *dst = px_color;
                });
            }
            pen_x += advance;
        }
    });
}

pub fn polygon(points: &[(f32, f32)], color: Rgba) {
    if points.len() < 3 {
        return;
    }
    with_canvas(|canvas| {
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0, points[0].1);
        for &(x, y) in &points[1..] {
            pb.line_to(x, y);
        }
        pb.close();
        let Some(path) = pb.finish() else { return };
        let paint = paint_for(color);
        canvas.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    });
}

/// draws an image file scaled into the (x, y, w, h) box. decoded once per
/// path and cached for the process lifetime (plugin overlays tend to draw
/// the same handful of icons every frame).
pub fn image(path: &str, x: f32, y: f32, w: f32, h: f32, opacity: f32, radius: f32) {
    thread_local! {
        static CACHE: RefCell<std::collections::HashMap<String, Option<Pixmap>>> =
            RefCell::new(std::collections::HashMap::new());
    }

    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let decoded = cache
            .entry(path.to_string())
            .or_insert_with(|| decode_image(path))
            .clone();
        let Some(src) = decoded else { return };
        with_canvas(|canvas| {
            if src.width() == 0 || src.height() == 0 {
                return;
            }
            let sx = w / src.width() as f32;
            let sy = h / src.height() as f32;
            let transform = Transform::from_scale(sx, sy).post_translate(x, y);
            let paint = tiny_skia::PixmapPaint {
                opacity: opacity.clamp(0.0, 1.0),
                ..Default::default()
            };
            // rounded corners: clip the draw with a mask of the rounded box
            let mask = if radius > 0.0 {
                rounded_rect_path(x, y, w, h, radius).and_then(|path| {
                    let mut mask = tiny_skia::Mask::new(canvas.width(), canvas.height())?;
                    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
                    Some(mask)
                })
            } else {
                None
            };
            canvas.draw_pixmap(0, 0, src.as_ref(), &paint, transform, mask.as_ref());
        });
    });
}

fn decode_image(path: &str) -> Option<Pixmap> {
    let img = ::image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut pixmap = Pixmap::new(w, h)?;
    // tiny-skia wants premultiplied RGBA8; `image`'s buffer is straight alpha.
    for (dst, px) in pixmap.data_mut().chunks_exact_mut(4).zip(img.pixels()) {
        let [r, g, b, a] = px.0;
        let af = a as f32 / 255.0;
        dst[0] = (r as f32 * af).round() as u8;
        dst[1] = (g as f32 * af).round() as u8;
        dst[2] = (b as f32 * af).round() as u8;
        dst[3] = a;
    }
    Some(pixmap)
}

/// the platform backend: wlr-layer-shell on Wayland, an ARGB window on X11
enum Backend {
    Wayland(wayland::WaylandOverlay),
    X11(x11::X11Overlay),
}

impl Backend {
    fn is_closed(&self) -> bool {
        match self {
            Backend::Wayland(o) => o.is_closed(),
            Backend::X11(o) => o.is_closed(),
        }
    }

    fn poll_size(&mut self) -> Option<(u32, u32)> {
        match self {
            Backend::Wayland(o) => o.poll_size(),
            Backend::X11(o) => o.poll_size(),
        }
    }

    fn present(&mut self, pixmap: &Pixmap) {
        match self {
            Backend::Wayland(o) => o.present(pixmap),
            Backend::X11(o) => o.present(pixmap),
        }
    }

    fn hide(&mut self) {
        match self {
            Backend::Wayland(o) => o.hide(),
            Backend::X11(o) => o.hide(),
        }
    }

    /// Wayland session -> layer-shell, otherwise X11 if there is a display
    fn create() -> Result<Self, String> {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        if wayland {
            return wayland::WaylandOverlay::new().map(Backend::Wayland);
        }
        if std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) {
            return x11::X11Overlay::new().map(Backend::X11);
        }
        Err("no WAYLAND_DISPLAY or DISPLAY set".to_string())
    }
}

/// the overlay window
pub struct Overlay {
    backend: Option<Backend>,
    last_reconnect: Option<std::time::Instant>,
}

impl Overlay {
    pub fn new() -> Self {
        match Backend::create() {
            Ok(backend) => {
                tracing::info!(
                    "game overlay: {} backend",
                    match backend {
                        Backend::Wayland(_) => "wlr-layer-shell",
                        Backend::X11(_) => "X11",
                    }
                );
                Overlay { backend: Some(backend), last_reconnect: None }
            }
            Err(e) => {
                tracing::warn!(
                    "overlay unavailable ({e}); drawing calls will be no-ops. This needs a \
                     Wayland compositor with wlr-layer-shell (Hyprland, Sway, ...), or an X11 \
                     session with a compositing manager (Cinnamon, XFCE, i3 + picom, ...)."
                );
                Overlay { backend: None, last_reconnect: Some(std::time::Instant::now()) }
            }
        }
    }

    /// paint one frame. `rect` (windows-style RL window bounds) is currently
    /// unused: the overlay always anchors full-screen per the wlr-layer-shell
    /// design (see module docs) rather than being positioned/sized to match
    /// a specific window like the old DirectComposition/GDI backends did.
    pub fn frame(&mut self, _rect: (i32, i32, i32, i32), draw_fn: impl FnOnce(f32, f32)) {
        // the wayland connection can die mid-session -- confirmed live to be
        // Hyprland occasionally sending our layer surface a fresh configure
        // (plausibly triggered by another layer client's exclusive zone
        // changing, e.g. waybar's own hide/show) that this backend doesn't
        // yet handle cleanly, reported by Hyprland as "layerSurface was not
        // configured, but a buffer was attached". Rather than chase that
        // compositor interaction further, rebuild the connection when it
        // happens so the overlay self-heals within about a second instead
        // of staying dark for the rest of the session. Rate-limited so a
        // connection that dies immediately on every reconnect (should the
        // trigger ever fire on every tick) can't turn into a tight loop.
        if self.backend.as_ref().is_some_and(|b| b.is_closed()) {
            let cooldown_elapsed = self
                .last_reconnect
                .map(|t| t.elapsed() > std::time::Duration::from_secs(1))
                .unwrap_or(true);
            if cooldown_elapsed {
                tracing::warn!("game overlay: connection died, reconnecting");
                self.last_reconnect = Some(std::time::Instant::now());
                self.backend = Backend::create().ok();
            } else {
                return;
            }
        }
        // X11: the compositing manager may be started after Hebnix, so keep
        // trying now and then when it was missing at startup
        if self.backend.is_none()
            && std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty())
            && self
                .last_reconnect
                .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(10))
        {
            self.last_reconnect = Some(std::time::Instant::now());
            self.backend = Backend::create().ok();
        }
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let Some((w, h)) = backend.poll_size() else {
            return;
        };
        let Some(mut pixmap) = Pixmap::new(w, h) else {
            return;
        };
        pixmap.fill(Color::TRANSPARENT);

        CANVAS.with(|c| *c.borrow_mut() = Some(pixmap));
        draw_fn(w as f32, h as f32);
        let pixmap = CANVAS.with(|c| c.borrow_mut().take());

        if let Some(pixmap) = pixmap {
            backend.present(&pixmap);
        }
    }

    pub fn hide(&mut self) {
        if let Some(backend) = self.backend.as_mut() {
            backend.hide();
        }
    }
}

impl Default for Overlay {
    fn default() -> Self {
        Self::new()
    }
}

/// hide the overlay now if it's visible. Safe from any thread on Windows
/// (raw HWND + ShowWindow); on Linux the overlay backend lives on the main
/// thread only, so this just delegates to the same `Overlay::hide` path via
/// a flag the main loop checks, keeping the call site (monitor.rs) unchanged.
static FORCE_HIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn enforce_hidden() {
    FORCE_HIDE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// consumed by the main loop each tick; true means `Overlay::hide()` should
/// be called (and the flag is cleared).
#[doc(hidden)]
pub fn take_force_hide() -> bool {
    FORCE_HIDE.swap(false, std::sync::atomic::Ordering::Relaxed)
}
