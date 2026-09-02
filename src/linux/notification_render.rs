//! Native notification centre and toast renderer.
//!
//! This surface stays independent from the notification transport. It renders
//! grouped state supplied by `NotificationCenter`, so a future DBus portal or
//! app-specific adapter can feed the same Mac-like tray without owning pixels.

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

use super::fonts::FontBook;
use crate::{
    notifications::{NotificationCenter, NotificationGroup, NotificationLevel},
    windowing::Rect,
};

pub const PANEL_WIDTH: i32 = 360;
pub const PANEL_MARGIN: i32 = 16;
pub const HEADER_HEIGHT: i32 = 58;
pub const GROUP_HEIGHT: i32 = 94;
pub const GROUP_GAP: i32 = 10;
pub const MAX_TOASTS: usize = 3;
pub const MAX_OPEN_GROUPS: usize = 5;

const PANEL: [u8; 4] = [239, 246, 252, 248];
const CARD: [u8; 4] = [255, 255, 255, 250];
const CARD_UNREAD: [u8; 4] = [226, 245, 252, 255];
const TEXT: [u8; 4] = [20, 34, 48, 255];
const MUTED: [u8; 4] = [88, 107, 122, 255];
const SUBTLE: [u8; 4] = [128, 145, 158, 255];
const ACCENT: [u8; 4] = [8, 127, 178, 255];
const DND: [u8; 4] = [120, 92, 162, 255];
const ERROR: [u8; 4] = [184, 52, 63, 255];
const WARNING: [u8; 4] = [173, 111, 22, 255];

/// Bounds of the floating notification surface.
pub fn panel_rect(work_area: Rect, open: bool, group_count: usize) -> Rect {
    let visible = if open {
        group_count.min(MAX_OPEN_GROUPS)
    } else {
        group_count.min(MAX_TOASTS)
    };
    let height = if open {
        HEADER_HEIGHT + (visible as i32) * (GROUP_HEIGHT + GROUP_GAP) + 16
    } else {
        (visible as i32) * (GROUP_HEIGHT + GROUP_GAP) + 8
    };
    Rect::new(
        work_area.right() - PANEL_WIDTH - PANEL_MARGIN,
        work_area.origin.y + crate::topbar::BAR_HEIGHT + 10,
        PANEL_WIDTH.min(work_area.size.width.saturating_sub(16)).max(0),
        height
            .min(
                work_area
                    .size
                    .height
                    .saturating_sub(crate::topbar::BAR_HEIGHT + 20),
            )
            .max(0),
    )
}

/// Bounds of one rendered notification group for pointer hit-testing.
pub fn group_rect(panel: Rect, index: usize, open: bool) -> Rect {
    let y = panel.origin.y + if open { HEADER_HEIGHT } else { 4 } + index as i32 * (GROUP_HEIGHT + GROUP_GAP);
    Rect::new(
        panel.origin.x + 8,
        y,
        (panel.size.width - 16).max(0),
        GROUP_HEIGHT,
    )
}

/// A compact centre renderer whose buffer is limited to the tray, not output size.
pub struct NotificationRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
}

impl Default for NotificationRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationRenderer {
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

    /// Render the notification centre or the short toast stack.
    pub fn element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        center: &NotificationCenter,
        now_ms: u64,
        open: bool,
        fonts: &FontBook,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let groups = center.grouped_at(now_ms);
        let panel = panel_rect(work_area, open, groups.len());
        if panel.size.width <= 0 || panel.size.height <= 0 {
            return None;
        }
        let width = panel.size.width.max(1);
        let height = panel.size.height.max(1);
        if self.sized_for != (width, height) {
            let mut context = self.buffer.render();
            context.resize((width, height));
            self.sized_for = (width, height);
        }

        {
            let mut context = self.buffer.render();
            let _ = context.draw(|pixels| {
                for pixel in pixels.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[0, 0, 0, 0]);
                }
                let local_panel = Rect::new(0, 0, width, height);
                fill_round_rect(pixels, width as usize, local_panel, 18, PANEL);
                let w = width as usize;

                if open {
                    draw_text(
                        pixels,
                        w,
                        Rect::new(18, 14, width - 36, 12),
                        TEXT,
                        "Notification Centre",
                    );
                    let status = if !center.preferences().enabled {
                        "Notifications are off"
                    } else if center.preferences().do_not_disturb {
                        "Do Not Disturb is on"
                    } else {
                        "Recent alerts"
                    };
                    draw_text(
                        pixels,
                        w,
                        Rect::new(18, 34, width - 36, 9),
                        if center.preferences().do_not_disturb {
                            DND
                        } else {
                            MUTED
                        },
                        status,
                    );
                }

                let limit = if open { MAX_OPEN_GROUPS } else { MAX_TOASTS };
                for (index, group) in groups.iter().take(limit).enumerate() {
                    let global = group_rect(panel, index, open);
                    let rect = Rect::new(
                        global.origin.x - panel.origin.x,
                        global.origin.y - panel.origin.y,
                        global.size.width,
                        global.size.height,
                    );
                    draw_group(pixels, w, rect, group, fonts);
                }

                if groups.is_empty() {
                    let message = if !center.preferences().enabled {
                        "Turn notifications on in Settings"
                    } else if center.preferences().do_not_disturb {
                        "Alerts are queued until Do Not Disturb ends"
                    } else {
                        "No recent notifications"
                    };
                    let text_w = fonts.text_width(message, 10.0);
                    fonts.draw_text(
                        pixels,
                        w,
                        Rect::new((width - text_w) / 2, (height / 2).max(30), text_w + 4, 14),
                        MUTED,
                        message,
                        10.0,
                    );
                }

                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((width, height)),
                )])
            });
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysPoint::from((panel.origin.x as f64, panel.origin.y as f64)),
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

fn draw_group(pixels: &mut [u8], width: usize, rect: Rect, group: &NotificationGroup, fonts: &FontBook) {
    let latest = group.latest();
    let Some(latest) = latest else { return };
    let unread = group.unread_count() > 0;
    fill_round_rect(pixels, width, rect, 12, if unread { CARD_UNREAD } else { CARD });

    let accent = level_colour(latest.level);
    fill_round_rect(
        pixels,
        width,
        Rect::new(rect.origin.x + 12, rect.origin.y + 16, 8, 8),
        4,
        accent,
    );
    draw_text(
        pixels,
        width,
        Rect::new(rect.origin.x + 28, rect.origin.y + 10, rect.size.width - 42, 9),
        MUTED,
        &truncate(&latest.source, 25),
    );
    draw_text(
        pixels,
        width,
        Rect::new(rect.origin.x + 28, rect.origin.y + 26, rect.size.width - 42, 11),
        TEXT,
        &truncate(&latest.title, 34),
    );
    draw_text(
        pixels,
        width,
        Rect::new(rect.origin.x + 28, rect.origin.y + 47, rect.size.width - 42, 10),
        MUTED,
        &truncate(&latest.body, 43),
    );
    if group.count() > 1 {
        let count = format!("{} alerts", group.count());
        draw_text(
            pixels,
            width,
            Rect::new(rect.origin.x + 28, rect.origin.y + 67, 100, 9),
            SUBTLE,
            &count,
        );
    }
    if unread {
        fill_round_rect(
            pixels,
            width,
            Rect::new(rect.right() - 28, rect.origin.y + 14, 8, 8),
            4,
            ACCENT,
        );
    }
    let _ = fonts;
}

fn level_colour(level: NotificationLevel) -> [u8; 4] {
    match level {
        NotificationLevel::Error | NotificationLevel::Critical => ERROR,
        NotificationLevel::Warning => WARNING,
        NotificationLevel::Success => [43, 158, 96, 255],
        NotificationLevel::Info => ACCENT,
    }
}

fn draw_text(pixels: &mut [u8], width: usize, rect: Rect, colour: [u8; 4], text: &str) {
    super::pixel::draw_text(pixels, width, rect, colour, text);
}

fn fill_round_rect(pixels: &mut [u8], width: usize, rect: Rect, radius: i32, colour: [u8; 4]) {
    super::pixel::fill_round_rect(pixels, width, rect, radius, colour);
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.pop();
        output.push('…');
    }
    output
}
