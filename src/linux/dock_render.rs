//! The Liquid Glass dock renderer.
//!
//! One `MemoryRenderBuffer` covers the dock's strip of the output. Each frame
//! the panel, icons, indicator dots and tooltip are painted from the pure
//! [`crate::dock::DockModel`] layout, then uploaded as a single render
//! element above all windows. Icons are procedurally drawn per application
//! until a real icon-theming pipeline lands.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportAll, ImportMem, Renderer,
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
        },
    },
    utils::Point as PhysPoint,
};

use super::pixel::{draw_text, fill_circle, fill_round_rect, text_width};
use crate::{
    dock::{DOT_RADIUS, DockItem, DockModel, MAX_ICON_SIZE, PANEL_RADIUS, TOOLTIP_OFFSET},
    windowing::{Point, Rect},
};

/// Ocean-tinted glass panel, matching the wallpaper palette.
const PANEL: [u8; 4] = [214, 228, 244, 165];
const PANEL_EDGE: [u8; 4] = [235, 244, 252, 90];
const DOT: [u8; 4] = [46, 58, 74, 230];
const TOOLTIP: [u8; 4] = [30, 36, 46, 235];
const TOOLTIP_TEXT: [u8; 4] = [244, 247, 251, 255];
const BADGE: [u8; 4] = [232, 92, 74, 245];
const BADGE_TEXT: [u8; 4] = [255, 255, 255, 255];

/// Height of the whole dock strip the buffer covers, including tooltip slack.
const STRIP_EXTRA: i32 = 72;

/// Which icon a pointer hovers this frame, for the tooltip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hover {
    pub index: usize,
}

/// Owns the dock's render surface.
pub struct DockRenderer {
    buffer: MemoryRenderBuffer,
    /// The work area the strip was last sized for.
    sized_for: (i32, i32),
}

impl DockRenderer {
    pub fn new() -> Self {
        Self {
            buffer: MemoryRenderBuffer::new(
                Fourcc::Argb8888,
                (8, 8),
                1,
                smithay::utils::Transform::Normal,
                None,
            ),
            sized_for: (0, 0),
        }
    }

    /// Paint the dock and produce its render element for this frame.
    ///
    /// `pointer` drives magnification; `hover` draws the tooltip;
    /// `bounces` maps item index to a launch-bounce lift in 0.0..1.0.
    pub fn render_element<R>(
        &mut self,
        renderer: &mut R,
        model: &DockModel,
        work_area: Rect,
        pointer: Option<Point>,
        hover: Option<Hover>,
        bounces: &dyn Fn(usize) -> f32,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let strip_height = MAX_ICON_SIZE + STRIP_EXTRA;
        let width = work_area.size.width.max(1);
        if self.sized_for != (width, strip_height) {
            let mut context = self.buffer.render();
            context.resize((width, strip_height));
            self.sized_for = (width, strip_height);
        }

        // The strip's origin in work-area coordinates: it ends at the work
        // area's bottom, which for the nested output is the output bottom.
        let strip_top = work_area.bottom() - strip_height;
        let strip_origin = (0, strip_top);

        {
            let mut context = self.buffer.render();
            let _ = context.draw(|pixels| {
                let frame = Paint {
                    model,
                    work_area,
                    strip_origin,
                    pointer,
                    hover,
                };
                paint(pixels, width as usize, frame, bounces);
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((width, strip_height)),
                )])
            });
        }

        let location = PhysPoint::from((0.0, strip_top as f64));
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location,
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

/// One frame of dock description, bundling what `paint` needs.
struct Paint<'a> {
    model: &'a DockModel,
    work_area: Rect,
    strip_origin: (i32, i32),
    pointer: Option<Point>,
    hover: Option<Hover>,
}

/// Paint the whole dock strip in one pass.
fn paint(
    pixels: &mut [u8],
    width: usize,
    Paint {
        model,
        work_area,
        strip_origin,
        pointer,
        hover,
    }: Paint<'_>,
    bounces: &dyn Fn(usize) -> f32,
) {
    // Clear the strip: fully transparent before painting.
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 0]);
    }

    let local = |rect: Rect| {
        Rect::new(
            rect.origin.x - strip_origin.0,
            rect.origin.y - strip_origin.1,
            rect.size.width,
            rect.size.height,
        )
    };

    let panel = local(model.panel(work_area, pointer));
    fill_round_rect(pixels, width, panel, PANEL_RADIUS, PANEL);
    // A brighter hairline along the panel's top edge, like glass catching light.
    let edge = Rect::new(
        panel.origin.x + PANEL_RADIUS,
        panel.origin.y,
        panel.size.width - 2 * PANEL_RADIUS,
        1,
    );
    fill_round_rect(pixels, width, edge, 0, PANEL_EDGE);

    for (index, item) in model.items().iter().enumerate() {
        let bounce = bounces(index);
        let geometry = model.icon_geometry(index, work_area, pointer, bounce);
        let icon = local(geometry.rect);
        draw_app_icon(pixels, width, icon, item);

        if item.running {
            let dot = model.dot_center(index, work_area, pointer);
            fill_circle(
                pixels,
                width,
                dot.x - strip_origin.0,
                dot.y - strip_origin.1,
                DOT_RADIUS,
                DOT,
            );
        }

        if item.minimized_windows > 0 {
            draw_badge(pixels, width, icon, item.minimized_windows);
        }
    }

    if let Some(hover) = hover {
        if let Some(item) = model.items().get(hover.index) {
            draw_tooltip(pixels, width, model, work_area, strip_origin, hover.index, item);
        }
    }
}

/// Procedural per-app icon: a squircle base tinted by a stable hash of the
/// app id, with a simple glyph motif derived from the label's first letter.
fn draw_app_icon(pixels: &mut [u8], width: usize, icon: Rect, item: &DockItem) {
    if icon.size.width <= 0 || icon.size.height <= 0 {
        return;
    }
    let tint = icon_tint(&item.app_id);
    fill_round_rect(pixels, width, icon, icon.size.width / 5, tint);

    // The motif: the first letter of the label, centred, in white.
    let letter = item
        .label
        .chars()
        .next()
        .unwrap_or('?')
        .to_ascii_uppercase()
        .to_string();
    let glyph_x = icon.origin.x + (icon.size.width - text_width(&letter)) / 2;
    let glyph_y = icon.origin.y + (icon.size.height - 7) / 2;
    draw_text(
        pixels,
        width,
        Rect::new(glyph_x, glyph_y, icon.size.width, 7),
        [255, 255, 255, 250],
        &letter,
    );
}

/// Stable per-app icon colour from the app id, biased to the ocean palette.
fn icon_tint(app_id: &str) -> [u8; 4] {
    let hash = app_id
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));

    const PALETTE: [[u8; 4]; 6] = [
        [12, 102, 160, 255],
        [22, 142, 175, 255],
        [72, 165, 190, 255],
        [36, 84, 141, 255],
        [94, 148, 178, 255],
        [23, 118, 141, 255],
    ];
    PALETTE[(hash % PALETTE.len() as u32) as usize]
}

/// The minimized-windows badge on an icon's top-right corner.
fn draw_badge(pixels: &mut [u8], width: usize, icon: Rect, count: usize) {
    let radius = 10;
    let cx = icon.right() - radius;
    let cy = icon.origin.y + radius;
    fill_circle(pixels, width, cx, cy, radius, BADGE);

    let label = if count > 9 {
        "9+".to_string()
    } else {
        count.to_string()
    };
    let label_w = text_width(&label);
    draw_text(
        pixels,
        width,
        Rect::new(cx - label_w / 2, cy - 3, label_w + 2, 7),
        BADGE_TEXT,
        &label,
    );
}

/// The macOS tooltip: a small dark rounded label above the hovered icon.
fn draw_tooltip(
    pixels: &mut [u8],
    width: usize,
    model: &DockModel,
    work_area: Rect,
    strip_origin: (i32, i32),
    index: usize,
    item: &DockItem,
) {
    let geometry = model.icon_geometry(index, work_area, None, 0.0);
    let label_w = text_width(&item.label).max(24) + 16;
    let label_h = 22;
    let cx = geometry.rect.origin.x + geometry.rect.size.width / 2;
    let top = geometry.rect.origin.y - TOOLTIP_OFFSET - label_h;

    let tooltip = Rect::new(
        cx - label_w / 2 - strip_origin.0,
        top - strip_origin.1,
        label_w,
        label_h,
    );
    fill_round_rect(pixels, width, tooltip, 6, TOOLTIP);
    let text_x = tooltip.origin.x + (tooltip.size.width - text_width(&item.label)) / 2;
    draw_text(
        pixels,
        width,
        Rect::new(
            text_x,
            tooltip.origin.y + (label_h - 7) / 2,
            tooltip.size.width,
            7,
        ),
        TOOLTIP_TEXT,
        &item.label,
    );
}
