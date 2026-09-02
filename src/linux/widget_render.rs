//! The Tahoe widget-rail renderer.
//!
//! Two `MemoryRenderBuffer` passes cover the board and the removal popup.
//! The board pass paints a transparent buffer sized to the board rect —
//! each tile's content is drawn from its pure [`crate::widgets`] geometry —
//! while the popup pass paints the whole work area and floats the
//! confirmation card under its widget, with the Cancel/Remove buttons
//! placed by `popup_buttons`.

// The desktop milestone wires this renderer into the frame loop; it ships
// compiled ahead of its callers.
#![allow(dead_code)]

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

use super::pixel::{draw_text, fill_circle, fill_rect, fill_round_rect, text_width};
use crate::topbar::clock_text;
use crate::widgets::{
    MINUS_RADIUS, TILE_RADIUS, WidgetBoard, WidgetKind, WidgetReading, WidgetSlot, popup_buttons,
};
use crate::windowing::Rect;

/// Ocean-tinted glass tile, matching the dock panel.
const TILE: [u8; 4] = [216, 228, 240, 190];
const TILE_TITLE: [u8; 4] = [176, 186, 198, 255];
const VALUE_TEXT: [u8; 4] = [244, 247, 251, 255];
const CAPTION_TEXT: [u8; 4] = [176, 186, 198, 255];
const BATTERY_TRACK: [u8; 4] = [52, 62, 74, 220];
const BATTERY_LEVEL: [u8; 4] = [46, 204, 113, 255];
const BATTERY_CHARGING: [u8; 4] = [46, 204, 113, 255];
const SUN: [u8; 4] = [247, 196, 87, 255];
const SPEAKER: [u8; 4] = [244, 247, 251, 230];
const SIGNAL: [u8; 4] = [244, 247, 251, 235];
const BAR_TRACK: [u8; 4] = [52, 62, 74, 220];
const BAR_STORAGE: [u8; 4] = [14, 118, 168, 255];
const BAR_CPU: [u8; 4] = [230, 126, 34, 255];
const BAR_MEMORY: [u8; 4] = [46, 204, 113, 255];
const EDIT_DIM: [u8; 4] = [0, 0, 0, 40];
const MINUS_FACE: [u8; 4] = [255, 255, 255, 255];
const MINUS_STROKE: [u8; 4] = [22, 30, 42, 255];
const MINUS_DASH: [u8; 4] = [22, 30, 42, 255];

const POPUP_SHADOW: [u8; 4] = [10, 14, 22, 60];
const POPUP_CARD: [u8; 4] = [242, 246, 250, 250];
const POPUP_TITLE: [u8; 4] = [22, 30, 42, 255];
const POPUP_TEXT: [u8; 4] = [52, 64, 80, 255];
const POPUP_CANCEL: [u8; 4] = [226, 230, 236, 255];
const POPUP_REMOVE: [u8; 4] = [232, 72, 61, 255];
const POPUP_BUTTON_TEXT: [u8; 4] = [255, 255, 255, 255];

/// The micro-font's glyph height, for vertical centring.
const GLYPH_HEIGHT: i32 = 7;

/// Renders the widget rail, edit-mode minus badges and the removal popup.
pub struct WidgetRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
}

impl WidgetRenderer {
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

    fn ensure_size(&mut self, width: i32, height: i32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.sized_for != (width, height) {
            let mut context = self.buffer.render();
            context.resize((width, height));
            self.sized_for = (width, height);
        }
    }

    /// Pinta o board inteiro (transparente onde não há tile) e retorna o
    /// elemento de render. `readings` resolve cada widget para texto/valor.
    pub fn board_element<R>(
        &mut self,
        renderer: &mut R,
        board: &WidgetBoard,
        board_rect: Rect,
        readings: &dyn Fn(WidgetKind) -> WidgetReading,
        now_secs: u64,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        self.ensure_size(board_rect.size.width, board_rect.size.height);
        {
            let mut context = self.buffer.render();
            let w = self.sized_for.0 as usize;
            let _ = context.draw(|pixels| {
                paint_board(pixels, w, board, board_rect, readings, now_secs);
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((self.sized_for.0, self.sized_for.1)),
                )])
            });
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysPoint::from((board_rect.origin.x as f64, board_rect.origin.y as f64)),
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }

    /// Pinta o popup de confirmação "Remove X?" com Cancel/Remove.
    pub fn popup_element<R>(
        &mut self,
        renderer: &mut R,
        popup: Rect,
        kind: WidgetKind,
        work_area: Rect,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        self.ensure_size(work_area.size.width, work_area.size.height);
        {
            let mut context = self.buffer.render();
            let w = self.sized_for.0 as usize;
            let _ = context.draw(|pixels| {
                paint_popup(pixels, w, popup, kind);
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((self.sized_for.0, self.sized_for.1)),
                )])
            });
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysPoint::from((0.0, 0.0)),
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

/// Paint the full board pass into its transparent buffer.
fn paint_board(
    pixels: &mut [u8],
    width: usize,
    board: &WidgetBoard,
    board_rect: Rect,
    readings: &dyn Fn(WidgetKind) -> WidgetReading,
    now_secs: u64,
) {
    clear(pixels);

    for slot in board.slots.iter() {
        let tile = local_rect(slot.rect(board_rect), board_rect.origin);
        fill_round_rect(pixels, width, tile, TILE_RADIUS, TILE);
        draw_tile_content(pixels, width, tile, *slot, readings, now_secs);

        let title_x = tile.origin.x + (tile.size.width - text_width(slot.kind.name())) / 2;
        draw_text(
            pixels,
            width,
            Rect::new(title_x, tile.origin.y + 10, tile.size.width, GLYPH_HEIGHT),
            TILE_TITLE,
            slot.kind.name(),
        );

        if board.editing {
            fill_round_rect(pixels, width, tile, TILE_RADIUS, EDIT_DIM);
            draw_minus_badge(pixels, width, tile);
        }
    }
}

/// Draw one widget's content inside its tile.
fn draw_tile_content(
    pixels: &mut [u8],
    width: usize,
    tile: Rect,
    slot: WidgetSlot,
    readings: &dyn Fn(WidgetKind) -> WidgetReading,
    now_secs: u64,
) {
    let reading = readings(slot.kind);

    match slot.kind {
        WidgetKind::Battery => {
            let body = Rect::new(tile.origin.x + 26, tile.origin.y + 58, 64, 28);
            fill_round_rect(pixels, width, body, 5, BATTERY_TRACK);
            let level_w = ((body.size.width - 6) as f32 * reading.value / 100.0) as i32;
            fill_round_rect(
                pixels,
                width,
                Rect::new(
                    body.origin.x + 3,
                    body.origin.y + 3,
                    level_w.max(0),
                    body.size.height - 6,
                ),
                3,
                BATTERY_LEVEL,
            );
            // The battery cap.
            fill_rect(
                pixels,
                width,
                Rect::new(body.right() + 3, body.origin.y + 9, 4, 10),
                BATTERY_TRACK,
            );
            draw_value_and_caption(
                pixels,
                width,
                tile,
                &format!("{}%", reading.value.round() as i32),
                if reading.caption.is_empty() {
                    "Battery"
                } else {
                    &reading.caption
                },
                reading.caption == "Charging",
            );
        }
        WidgetKind::Brightness => {
            let sun = (tile.origin.x + 44, tile.origin.y + 70);
            fill_circle(pixels, width, sun.0, sun.1, 16, SUN);
            let rays = [
                (0, -24),
                (0, 24),
                (-24, 0),
                (24, 0),
                (-17, -17),
                (17, -17),
                (-17, 17),
                (17, 17),
            ];
            for (dx, dy) in rays {
                fill_circle(pixels, width, sun.0 + dx, sun.1 + dy, 2, SUN);
            }
            draw_value_and_caption(
                pixels,
                width,
                tile,
                &format!("{}%", reading.value.round() as i32),
                &reading.caption,
                false,
            );
        }
        WidgetKind::Volume => {
            let base_x = tile.origin.x + 22;
            let base_y = tile.origin.y + 66;
            // The speaker cabinet: a small square plus a cone built from
            // stacked bars widening away from it.
            fill_rect(pixels, width, Rect::new(base_x, base_y + 8, 10, 12), SPEAKER);
            fill_rect(pixels, width, Rect::new(base_x + 10, base_y + 10, 4, 8), SPEAKER);
            fill_rect(pixels, width, Rect::new(base_x + 14, base_y + 7, 4, 14), SPEAKER);
            fill_rect(pixels, width, Rect::new(base_x + 18, base_y + 3, 4, 22), SPEAKER);
            draw_value_and_caption(
                pixels,
                width,
                tile,
                &format!("{}%", reading.value.round() as i32),
                &reading.caption,
                false,
            );
        }
        WidgetKind::Network => {
            // Simplified Wi-Fi mark: three signal dots rising from the
            // bottom-left corner of the content area.
            let origin_x = tile.origin.x + 24;
            let bottom_y = tile.origin.y + 86;
            let radii = [2, 4, 6];
            for (i, radius) in radii.iter().enumerate() {
                let step = i as i32;
                fill_circle(
                    pixels,
                    width,
                    origin_x + step * 14,
                    bottom_y - step * 12,
                    *radius,
                    SIGNAL,
                );
            }
            let caption_x = tile.origin.x + (tile.size.width - text_width(&reading.caption)) / 2;
            draw_text(
                pixels,
                width,
                Rect::new(caption_x, tile.origin.y + 120, tile.size.width, GLYPH_HEIGHT),
                CAPTION_TEXT,
                &reading.caption,
            );
        }
        WidgetKind::Clock => {
            let label = clock_text(now_secs);
            let label_w = text_width(&label);
            draw_text(
                pixels,
                width,
                Rect::new(
                    tile.origin.x + (tile.size.width - label_w) / 2,
                    tile.origin.y + 52,
                    label_w + 4,
                    GLYPH_HEIGHT,
                ),
                VALUE_TEXT,
                &label,
            );
            let caption = "Rouch";
            let caption_w = text_width(caption);
            draw_text(
                pixels,
                width,
                Rect::new(
                    tile.origin.x + (tile.size.width - caption_w) / 2,
                    tile.origin.y + 76,
                    caption_w + 4,
                    GLYPH_HEIGHT,
                ),
                CAPTION_TEXT,
                caption,
            );
        }
        WidgetKind::Storage | WidgetKind::Cpu | WidgetKind::Memory => {
            let fill_colour = match slot.kind {
                WidgetKind::Storage => BAR_STORAGE,
                WidgetKind::Cpu => BAR_CPU,
                _ => BAR_MEMORY,
            };
            let track = Rect::new(tile.origin.x + 24, tile.origin.y + 62, tile.size.width - 48, 10);
            fill_round_rect(pixels, width, track, 5, BAR_TRACK);
            let fill_w = (track.size.width as f32 * reading.value / 100.0) as i32;
            fill_round_rect(
                pixels,
                width,
                Rect::new(
                    track.origin.x,
                    track.origin.y,
                    fill_w.max(0).min(track.size.width),
                    track.size.height,
                ),
                5,
                fill_colour,
            );
            let percent = format!("{}%", reading.value.round() as i32);
            let percent_w = text_width(&percent);
            draw_text(
                pixels,
                width,
                Rect::new(
                    track.right() - percent_w,
                    track.origin.y - 14,
                    percent_w + 4,
                    GLYPH_HEIGHT,
                ),
                VALUE_TEXT,
                &percent,
            );
            let caption_x = tile.origin.x + (tile.size.width - text_width(&reading.caption)) / 2;
            draw_text(
                pixels,
                width,
                Rect::new(caption_x, tile.origin.y + 92, tile.size.width, GLYPH_HEIGHT),
                CAPTION_TEXT,
                &reading.caption,
            );
        }
    }
}

/// The shared percent-plus-caption pattern most tiles follow.
fn draw_value_and_caption(
    pixels: &mut [u8],
    width: usize,
    tile: Rect,
    value: &str,
    caption: &str,
    charging: bool,
) {
    let value_w = text_width(value);
    draw_text(
        pixels,
        width,
        Rect::new(
            tile.origin.x + (tile.size.width - value_w) / 2,
            tile.origin.y + 96,
            value_w + 4,
            GLYPH_HEIGHT,
        ),
        VALUE_TEXT,
        value,
    );
    let caption_colour = if charging { BATTERY_CHARGING } else { CAPTION_TEXT };
    let caption_w = text_width(caption);
    draw_text(
        pixels,
        width,
        Rect::new(
            tile.origin.x + (tile.size.width - caption_w) / 2,
            tile.origin.y + 116,
            caption_w + 4,
            GLYPH_HEIGHT,
        ),
        caption_colour,
        caption,
    );
}

/// The edit-mode minus badge: a white circle with a dark rim in the tile's
/// top-left corner, matching `WidgetBoard::minus_center`'s hit geometry.
fn draw_minus_badge(pixels: &mut [u8], width: usize, tile: Rect) {
    let cx = tile.origin.x + 4 + MINUS_RADIUS;
    let cy = tile.origin.y + 4 + MINUS_RADIUS;
    // The rim is the dark circle behind the face; the face sits inside it.
    let rim = MINUS_RADIUS + 2;
    fill_circle(pixels, width, cx, cy, rim, MINUS_STROKE);
    fill_circle(pixels, width, cx, cy, MINUS_RADIUS, MINUS_FACE);
    fill_rect(pixels, width, Rect::new(cx - 5, cy - 1, 10, 2), MINUS_DASH);
}

/// Paint the removal-popup pass into a work-area-sized transparent buffer.
fn paint_popup(pixels: &mut [u8], width: usize, popup: Rect, kind: WidgetKind) {
    clear(pixels);

    // The card's shadow, offset like the update card's.
    let shadow = Rect::new(popup.origin.x + 10, popup.bottom(), popup.size.width - 20, 8);
    fill_round_rect(pixels, width, shadow, 4, POPUP_SHADOW);

    fill_round_rect(pixels, width, popup, 16, POPUP_CARD);

    let title = format!("Remove \"{}\"?", kind.name());
    let title_w = text_width(&title);
    draw_text(
        pixels,
        width,
        Rect::new(
            popup.origin.x + (popup.size.width - title_w) / 2,
            popup.origin.y + 22,
            title_w + 4,
            GLYPH_HEIGHT,
        ),
        POPUP_TITLE,
        &title,
    );

    // Subtext: split into two lines if it doesn't fit on one.
    let line = "Rouch will remove this widget from the board.";
    let max_w = popup.size.width - 32;
    if text_width(line) <= max_w {
        let line_w = text_width(line);
        draw_text(
            pixels,
            width,
            Rect::new(
                popup.origin.x + (popup.size.width - line_w) / 2,
                popup.origin.y + 46,
                line_w + 4,
                GLYPH_HEIGHT,
            ),
            POPUP_TEXT,
            line,
        );
    } else {
        let first = "Rouch will remove this";
        let second = "widget from the board.";
        let first_w = text_width(first);
        let second_w = text_width(second);
        draw_text(
            pixels,
            width,
            Rect::new(
                popup.origin.x + (popup.size.width - first_w) / 2,
                popup.origin.y + 46,
                first_w + 4,
                GLYPH_HEIGHT,
            ),
            POPUP_TEXT,
            first,
        );
        draw_text(
            pixels,
            width,
            Rect::new(
                popup.origin.x + (popup.size.width - second_w) / 2,
                popup.origin.y + 60,
                second_w + 4,
                GLYPH_HEIGHT,
            ),
            POPUP_TEXT,
            second,
        );
    }

    let (cancel, remove) = popup_buttons(popup);
    draw_button(pixels, width, cancel, POPUP_CANCEL, POPUP_TITLE, "Cancel");
    draw_button(pixels, width, remove, POPUP_REMOVE, POPUP_BUTTON_TEXT, "Remove");
}

/// One popup button: a pill with centred label.
fn draw_button(
    pixels: &mut [u8],
    width: usize,
    button: Rect,
    face: [u8; 4],
    label_colour: [u8; 4],
    label: &str,
) {
    fill_round_rect(pixels, width, button, 12, face);
    let label_w = text_width(label);
    draw_text(
        pixels,
        width,
        Rect::new(
            button.origin.x + (button.size.width - label_w) / 2,
            button.origin.y + (button.size.height - GLYPH_HEIGHT) / 2,
            label_w + 4,
            GLYPH_HEIGHT,
        ),
        label_colour,
        label,
    );
}

/// Zero the buffer to fully transparent.
fn clear(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 0]);
    }
}

/// Translate a work-area rect into the board's local coordinates.
fn local_rect(rect: Rect, origin: crate::windowing::Point) -> Rect {
    Rect::new(
        rect.origin.x - origin.x,
        rect.origin.y - origin.y,
        rect.size.width,
        rect.size.height,
    )
}
