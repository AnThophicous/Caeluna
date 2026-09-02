//! Rouch-drawn window chrome: the translucent title bar and traffic lights.
//!
//! Each window owns one `MemoryRenderBuffer` the width of its frame and the
//! height of the title bar. Pixels are laid out BGRA in memory, which is what
//! `Fourcc::Argb8888` means for the GLES renderer. Redrawing is incremental:
//! a window that has not resized, focused, maximized or retitled since the
//! last frame does not upload anything. Moving a window only changes the
//! element transform, so it deliberately does not dirty the texture.
//!
//! To measure this path in a live nested session, run with
//! `RUST_LOG=rouch=trace`: Smithay's memory importer logs an update only when
//! damage is submitted. Moving an unchanged window should therefore produce
//! no chrome texture upload; changing its title or focus should produce one.

use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportAll, ImportMem, Renderer,
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::WaylandSurfaceRenderElement,
            },
        },
    },
    utils::{Physical, Point, Rectangle, Transform},
};

use crate::{
    chrome::{TITLE_BAR_HEIGHT, TrafficLightKind},
    windowing::{Rect, WindowId},
};

// One render pass element: either a client surface tree or Rouch chrome.
smithay::render_elements! {
    pub RouchRenderElements<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Chrome=MemoryRenderBufferRenderElement<R>,
}

/// Ocean-tinted translucent chrome, straight from the wallpaper palette.
const CHROME_ACTIVE: [u8; 4] = [246, 248, 252, 236];
const CHROME_INACTIVE: [u8; 4] = [238, 240, 244, 200];
const TITLE_TEXT: [u8; 4] = [24, 28, 36, 216];
const TITLE_TEXT_INACTIVE: [u8; 4] = [110, 116, 128, 190];
const HAIRLINE: [u8; 4] = [12, 16, 24, 36];

/// Keep a malformed or hostile window request from allocating an unbounded
/// software texture. Normal desktop outputs are far below this limit; an
/// oversized request still gets a small, safe chrome fallback and the client
/// content remains renderable.
const MAX_CHROME_WIDTH: i32 = 16_384;
const MAX_TITLE_CHARS: usize = 256;
const MAX_ANIMATION_SCALE: f32 = 4.0;
const MAX_RENDER_COORDINATE: f64 = 1_000_000_000.0;

const BLANK_GLYPH: [u128; 7] = [0; 7];

/// One window's cached chrome texture and the inputs it was drawn from.
struct ChromeSurface {
    buffer: MemoryRenderBuffer,
    dirty: bool,
    last_width: i32,
    last_active: bool,
    last_maximized: bool,
    last_title_key: u64,
}

impl ChromeSurface {
    fn new(frame: Rect) -> Self {
        let width = chrome_width(frame);
        Self {
            buffer: buffer_for(frame),
            dirty: true,
            last_width: width,
            last_active: false,
            last_maximized: false,
            last_title_key: title_key(""),
        }
    }

    fn mark_dirty(&mut self, frame: Rect, active: bool, maximized: bool, title: &str) {
        let width = chrome_width(frame);
        let title_key = title_key(title);
        let resized = width != self.last_width;
        if resized {
            // The render context owns the resize; it resets damage cleanly.
            let mut context = self.buffer.render();
            context.resize((width, TITLE_BAR_HEIGHT));
        }

        if resized
            || self.last_active != active
            || self.last_maximized != maximized
            || self.last_title_key != title_key
        {
            self.dirty = true;
        }
        self.last_width = width;
        self.last_active = active;
        self.last_maximized = maximized;
        self.last_title_key = title_key;
    }

    fn repaint(&mut self, active: bool, title: &str, maximized: bool) {
        if !self.dirty {
            return;
        }
        let mut context = self.buffer.render();
        let width = self.last_width as usize;

        let painted = context.draw(|pixels| {
            let chrome = if active { CHROME_ACTIVE } else { CHROME_INACTIVE };
            let text = if active { TITLE_TEXT } else { TITLE_TEXT_INACTIVE };

            // The bar wash, with a hairline border on its last row.
            let height = TITLE_BAR_HEIGHT as usize;
            for y in 0..height {
                let base = if y + 1 == height { HAIRLINE } else { chrome };
                let Some(row_start) = y.checked_mul(width).and_then(|offset| offset.checked_mul(4)) else {
                    return Result::<_, ()>::Err(());
                };
                let Some(row_end) = (y + 1)
                    .checked_mul(width)
                    .and_then(|offset| offset.checked_mul(4))
                else {
                    return Result::<_, ()>::Err(());
                };
                let Some(row) = pixels.get_mut(row_start..row_end) else {
                    return Result::<_, ()>::Err(());
                };
                for px in row.chunks_exact_mut(4) {
                    px.copy_from_slice(&base);
                }
            }

            draw_lights(pixels, width, active);
            draw_title(pixels, width, text, title, maximized);

            Result::<_, ()>::Ok(vec![Rectangle::from_size(smithay::utils::Size::from((
                width as i32,
                TITLE_BAR_HEIGHT,
            )))])
        });
        if painted.is_ok() {
            self.dirty = false;
        }
    }
}

fn buffer_for(frame: Rect) -> MemoryRenderBuffer {
    MemoryRenderBuffer::new(
        Fourcc::Argb8888,
        (chrome_width(frame), TITLE_BAR_HEIGHT),
        1,
        Transform::Normal,
        None,
    )
}

#[inline]
fn chrome_width(frame: Rect) -> i32 {
    frame.size.width.clamp(1, MAX_CHROME_WIDTH)
}

/// Blit the three traffic lights with a crisp 1px darker rim.
fn draw_lights(pixels: &mut [u8], width: usize, active: bool) {
    for (index, kind) in TrafficLightKind::ORDER.into_iter().enumerate() {
        let color: [f32; 4] = if active {
            kind.color()
        } else {
            kind.inactive_color()
        };
        let rgba = [
            (color[0] * 255.0).round() as u8,
            (color[1] * 255.0).round() as u8,
            (color[2] * 255.0).round() as u8,
            (color[3] * 255.0).round() as u8,
        ];
        let rim = [
            rgba[0].saturating_sub(46),
            rgba[1].saturating_sub(46),
            rgba[2].saturating_sub(46),
            255,
        ];

        let r = crate::chrome::TRAFFIC_LIGHT_DIAMETER as f32 / 2.0;
        // Work in local buffer coordinates. Apart from being cheaper, this
        // avoids overflowing global `Rect` arithmetic for a bad client frame.
        let cx = (crate::chrome::TRAFFIC_LIGHT_MARGIN
            + crate::chrome::TRAFFIC_LIGHT_DIAMETER / 2
            + index as i32 * crate::chrome::TRAFFIC_LIGHT_SPACING) as f32;
        let cy = (TITLE_BAR_HEIGHT / 2) as f32;

        let min_y = (cy - r - 1.0).floor().max(0.0) as usize;
        let max_y = (cy + r + 1.0).ceil().min(TITLE_BAR_HEIGHT as f32) as usize;
        let min_x = (cx - r - 1.0).floor().max(0.0) as usize;
        let max_x = (cx + r + 1.0).ceil().min(width as f32) as usize;

        for y in min_y..max_y {
            for x in min_x..max_x {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let px = rgba_or_rim(dist, r, rgba, rim);
                if let Some(rgba_px) = px {
                    let offset = (y * width + x) * 4;
                    if let Some(pixel) = pixels.get_mut(offset..offset.saturating_add(4)) {
                        pixel.copy_from_slice(&rgba_px);
                    }
                }
            }
        }
    }
}

fn rgba_or_rim(dist: f32, r: f32, fill: [u8; 4], rim: [u8; 4]) -> Option<[u8; 4]> {
    if dist <= r {
        Some(fill)
    } else if dist <= r + 1.0 {
        Some(rim)
    } else {
        None
    }
}

/// Render the window title with a tiny built-in font, centered in the bar.
///
/// A compositor cannot depend on fontconfig yet; a 5x7 micro-font covers
/// printable ASCII and draws crisp at logical 1:1. The dock/shell milestone
/// replaces this with a real font stack.
fn draw_title(pixels: &mut [u8], width: usize, color: [u8; 4], title: &str, maximized: bool) {
    if title.is_empty() {
        return;
    }

    let glyph_width = 5;
    let advance = glyph_width + 1;
    let glyph_count = title.chars().take(MAX_TITLE_CHARS).count();
    if glyph_count == 0 {
        return;
    }
    let text_width = glyph_count.saturating_mul(advance);
    let bar_center = width / 2;
    let mut start = bar_center.saturating_sub(text_width / 2);

    // Keep the title clear of the traffic lights on very narrow windows.
    let lights_zone = 90;
    if maximized && start < lights_zone {
        start = lights_zone;
    }
    if start + text_width + 4 > width {
        return;
    }

    let top = 13;
    for (i, character) in title.chars().take(MAX_TITLE_CHARS).enumerate() {
        // Non-ASCII and unhandled punctuation fold to a blank column, so a
        // UTF-8 title never renders mojibake in the micro-font.
        let glyph = glyph(character as u8).unwrap_or(&BLANK_GLYPH);
        let x0 = start + i * advance;
        for (row, bits) in glyph.iter().copied().take(7).enumerate() {
            for col in 0..glyph_width {
                if bits & (1 << (glyph_width - 1 - col)) != 0 {
                    let x = x0 + col;
                    let y = top + row;
                    if x < width && y < TITLE_BAR_HEIGHT as usize {
                        let offset = (y * width + x) * 4;
                        blend(pixels, offset, color);
                    }
                }
            }
        }
    }
}

/// Source-over blend of `color` into BGRA `pixels` at `offset`.
fn blend(pixels: &mut [u8], offset: usize, color: [u8; 4]) {
    let Some(end) = offset.checked_add(4) else {
        return;
    };
    let Some(pixel) = pixels.get_mut(offset..end) else {
        return;
    };
    let alpha = color[3] as u32;
    let inv = 255 - alpha;

    // BGRA in memory; our colour constants are stored BGRA too.
    for i in 0..4 {
        let src = color[i] as u32 * alpha;
        let base = pixel[i] as u32 * inv;
        pixel[i] = ((src + base + 127) / 255) as u8;
    }
}

/// A small allocation-free fingerprint for the part of a title we can draw.
/// Titles are client-owned strings, so retaining the full value would make a
/// repaint allocate even when only the window position changed.
fn title_key(title: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    let mut characters = title.chars();
    let mut count = 0usize;
    for character in characters.by_ref().take(MAX_TITLE_CHARS) {
        let mut encoded = [0u8; 4];
        for byte in character.encode_utf8(&mut encoded).bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        count += 1;
    }
    hash ^= count as u64;
    hash = hash.wrapping_mul(0x100000001b3);
    if characters.next().is_some() {
        hash ^= 1;
    }
    hash
}

/// Reject bad animation data at the renderer boundary. Invalid effects fall
/// back to a normal, fully opaque chrome element so client content is never
/// made invisible by a NaN, infinity, or out-of-range alpha.
fn safe_animation_sample(sample: AnimationSample) -> Option<AnimationSample> {
    let (scale, opacity, centre) = sample;
    (scale.is_finite()
        && scale > 0.0
        && scale <= MAX_ANIMATION_SCALE
        && opacity.is_finite()
        && (0.0..=1.0).contains(&opacity)
        && centre.x.is_finite()
        && centre.y.is_finite()
        && centre.x.abs() <= MAX_RENDER_COORDINATE
        && centre.y.abs() <= MAX_RENDER_COORDINATE)
        .then_some(sample)
}

type AnimatedGeometry = (
    f32,
    Point<f64, Physical>,
    smithay::utils::Size<i32, smithay::utils::Logical>,
);

fn animated_geometry(frame: Rect, sample: Option<AnimationSample>) -> Option<AnimatedGeometry> {
    let (scale, opacity, centre) = safe_animation_sample(sample?)?;
    let width = (f64::from(chrome_width(frame)) * f64::from(scale))
        .round()
        .clamp(1.0, f64::from(MAX_CHROME_WIDTH));
    let height = (f64::from(TITLE_BAR_HEIGHT) * f64::from(scale))
        .round()
        .clamp(1.0, f64::from(TITLE_BAR_HEIGHT * 4));
    let frame_height = f64::from(frame.size.height.max(0));
    let top_left = (
        centre.x - width / 2.0,
        centre.y - frame_height * f64::from(scale) / 2.0,
    );
    if !top_left.0.is_finite() || !top_left.1.is_finite() {
        return None;
    }

    Some((
        opacity,
        Point::<f64, Physical>::from(top_left),
        smithay::utils::Size::<i32, smithay::utils::Logical>::from((width as i32, height as i32)),
    ))
}

// A 5x7 micro-font of printable ASCII. Each glyph is seven rows of five
// bits, most significant bit leftmost in every row.
const GLYPHS: [(u8, [u128; 7]); 73] = [
    (b'A', [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11]),
    (b'B', [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E]),
    (b'C', [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E]),
    (b'D', [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C]),
    (b'E', [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F]),
    (b'F', [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10]),
    (b'G', [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F]),
    (b'H', [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11]),
    (b'I', [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    (b'J', [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C]),
    (b'K', [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11]),
    (b'L', [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F]),
    (b'M', [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11]),
    (b'N', [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11]),
    (b'O', [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E]),
    (b'P', [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10]),
    (b'Q', [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D]),
    (b'R', [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11]),
    (b'S', [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E]),
    (b'T', [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04]),
    (b'U', [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E]),
    (b'V', [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04]),
    (b'W', [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A]),
    (b'X', [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11]),
    (b'Y', [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04]),
    (b'Z', [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F]),
    (b'a', [0x00, 0x00, 0x0E, 0x01, 0x0F, 0x11, 0x0F]),
    (b'b', [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x1E]),
    (b'c', [0x00, 0x00, 0x0F, 0x10, 0x10, 0x10, 0x0F]),
    (b'd', [0x01, 0x01, 0x0F, 0x11, 0x11, 0x11, 0x0F]),
    (b'e', [0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E]),
    (b'f', [0x06, 0x08, 0x1C, 0x08, 0x08, 0x08, 0x08]),
    (b'g', [0x00, 0x0F, 0x11, 0x11, 0x0F, 0x01, 0x0E]),
    (b'h', [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x11]),
    (b'i', [0x04, 0x00, 0x0C, 0x04, 0x04, 0x04, 0x0E]),
    (b'j', [0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0C]),
    (b'k', [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12]),
    (b'l', [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    (b'm', [0x00, 0x00, 0x1A, 0x15, 0x15, 0x15, 0x15]),
    (b'n', [0x00, 0x00, 0x1E, 0x11, 0x11, 0x11, 0x11]),
    (b'o', [0x00, 0x00, 0x0E, 0x11, 0x11, 0x11, 0x0E]),
    (b'p', [0x00, 0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10]),
    (b'q', [0x00, 0x0F, 0x11, 0x11, 0x0F, 0x01, 0x01]),
    (b'r', [0x00, 0x00, 0x16, 0x09, 0x08, 0x08, 0x08]),
    (b's', [0x00, 0x00, 0x0F, 0x10, 0x0E, 0x01, 0x1E]),
    (b't', [0x08, 0x08, 0x1C, 0x08, 0x08, 0x08, 0x06]),
    (b'u', [0x00, 0x00, 0x11, 0x11, 0x11, 0x11, 0x0F]),
    (b'v', [0x00, 0x00, 0x11, 0x11, 0x11, 0x0A, 0x04]),
    (b'w', [0x00, 0x00, 0x11, 0x15, 0x15, 0x15, 0x0A]),
    (b'x', [0x00, 0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11]),
    (b'y', [0x00, 0x11, 0x11, 0x11, 0x0F, 0x01, 0x0E]),
    (b'z', [0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F]),
    (b'0', [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E]),
    (b'1', [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    (b'2', [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F]),
    (b'3', [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E]),
    (b'4', [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02]),
    (b'5', [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E]),
    (b'6', [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E]),
    (b'7', [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08]),
    (b'8', [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E]),
    (b'9', [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C]),
    (b' ', [0, 0, 0, 0, 0, 0, 0]),
    (b'.', [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C]),
    (b',', [0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08]),
    (b'-', [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00]),
    (b':', [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00]),
    (b'/', [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10]),
    (b'_', [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F]),
    (b'(', [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02]),
    (b')', [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08]),
    (b'!', [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04]),
    (b'?', [0x0E, 0x11, 0x01, 0x06, 0x04, 0x00, 0x04]),
];

/// Look up the rows for one ASCII byte, or None for unhandled bytes.
fn glyph(byte: u8) -> Option<&'static [u128; 7]> {
    GLYPHS.iter().find(|(key, _)| *key == byte).map(|(_, rows)| rows)
}

/// An animation sample for one window: scale, opacity and animated centre.
pub type AnimationSample = (f32, f32, Point<f64, smithay::utils::Logical>);

/// Owns every mapped window's chrome surface.
#[derive(Default)]
pub struct Decorations {
    surfaces: HashMap<WindowId, ChromeSurface>,
}

impl Decorations {
    pub fn forget(&mut self, id: WindowId) {
        self.surfaces.remove(&id);
    }

    /// Produce the chrome render elements for one frame, in the same window
    /// order the caller provides. The caller interleaves these with client
    /// surface elements so chrome always overlays its own client.
    ///
    /// `samples` carries the animated scale and opacity for each window; a
    /// `None` sample renders the chrome at rest.
    pub fn render_elements<R>(
        &mut self,
        renderer: &mut R,
        windows: &[(WindowId, Rect, bool, bool, &str)],
        samples: &dyn Fn(WindowId) -> Option<AnimationSample>,
    ) -> Vec<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let mut elements = Vec::with_capacity(windows.len());

        for &(id, frame, active, maximized, title) in windows {
            let surface = self
                .surfaces
                .entry(id)
                .or_insert_with(|| ChromeSurface::new(frame));
            surface.mark_dirty(frame, active, maximized, title);
            surface.repaint(active, title, maximized);

            let (alpha, location, size) = match animated_geometry(frame, samples(id)) {
                Some((alpha, location, size)) => (alpha, location, Some(size)),
                None => (
                    1.0,
                    Point::<f64, Physical>::from((frame.origin.x as f64, frame.origin.y as f64)),
                    None,
                ),
            };

            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location,
                &surface.buffer,
                if alpha < 1.0 { Some(alpha) } else { None },
                None,
                size,
                Kind::Unspecified,
            ) {
                elements.push(element);
            }
        }

        self.surfaces
            .retain(|id, _| windows.iter().any(|(window_id, ..)| window_id == id));
        elements
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_width_is_bounded_for_invalid_geometry() {
        assert_eq!(chrome_width(Rect::new(0, 0, -10, 20)), 1);
        assert_eq!(chrome_width(Rect::new(0, 0, 20_000, 20)), MAX_CHROME_WIDTH);
        assert_eq!(chrome_width(Rect::new(0, 0, 640, 20)), 640);
    }

    #[test]
    fn invalid_effect_values_fall_back_to_opaque_rest_state() {
        let centre = Point::<f64, smithay::utils::Logical>::from((10.0, 10.0));
        assert!(safe_animation_sample((f32::NAN, 1.0, centre)).is_none());
        assert!(safe_animation_sample((1.0, f32::INFINITY, centre)).is_none());
        assert!(safe_animation_sample((1.0, -0.1, centre)).is_none());
        assert!(safe_animation_sample((1.0, 1.0, centre)).is_some());
        let far_away = Point::<f64, smithay::utils::Logical>::from((f64::MAX, 0.0));
        assert!(safe_animation_sample((1.0, 1.0, far_away)).is_none());
    }

    #[test]
    fn animated_geometry_never_emits_zero_sized_elements() {
        let centre = Point::<f64, smithay::utils::Logical>::from((10.0, 10.0));
        let geometry = animated_geometry(Rect::new(0, 0, 200, 120), Some((0.001, 1.0, centre)));
        assert!(geometry.as_ref().is_some(), "valid animation sample rejected");
        if let Some((_, _, size)) = geometry {
            assert!(size.w >= 1);
            assert!(size.h >= 1);
        }
    }
}
