//! The Settings window renderer, macOS System Settings style.
//!
//! One transparent output-sized buffer carries the whole window: the glass
//! card with traffic-light dots, the sidebar of panes and the active pane's
//! rows — toggles, sliders, selects, infos, actions and the user card.

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
use crate::settings::{Pane, SIDEBAR_ROW, Setting, SettingTone, SettingsFocus, SettingsLayout};
use crate::user::{AvatarMark, UserProfile};
use crate::windowing::Rect;

const CARD: [u8; 4] = [246, 252, 240, 248];
const CARD_OPAQUE: [u8; 4] = [246, 252, 240, 255];
const SIDEBAR: [u8; 4] = [242, 234, 228, 255];
const ROW: [u8; 4] = [255, 254, 252, 255];
const TEXT: [u8; 4] = [42, 30, 22, 255];
const MUTED: [u8; 4] = [112, 100, 90, 255];
const ACCENT: [u8; 4] = [168, 118, 14, 255];
const ACCENT_SOFT: [u8; 4] = [252, 240, 224, 255];
const TRACK: [u8; 4] = [228, 222, 216, 255];
const KNOB_OFF: [u8; 4] = [196, 188, 180, 255];
const SHADOW: [u8; 4] = [60, 22, 14, 60];
const FOCUS: [u8; 4] = [38, 112, 214, 255];
const GAME_ACCENT: [u8; 4] = [43, 112, 202, 255];

const LIGHT_RED: [u8; 4] = [39, 48, 186, 255];
const LIGHT_AMBER: [u8; 4] = [25, 160, 153, 255];
const LIGHT_GREEN: [u8; 4] = [59, 165, 59, 255];

/// Owns the settings window surface.
pub struct SettingsRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
    focus: Option<SettingsFocus>,
    opaque_fallback: bool,
}

impl Default for SettingsRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsRenderer {
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
            focus: Some(SettingsFocus::Sidebar(0)),
            opaque_fallback: false,
        }
    }

    /// Keep a visible keyboard focus ring without coupling the model to
    /// renderer coordinates.
    pub fn set_focus(&mut self, focus: Option<SettingsFocus>) {
        self.focus = focus;
    }

    /// Use a fully opaque material when the user or the performance policy
    /// asks for reduced transparency/low-cost rendering.
    pub fn set_opaque_fallback(&mut self, opaque: bool) {
        self.opaque_fallback = opaque;
    }

    /// Paint the settings window over a transparent output buffer.
    pub fn element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        layout: SettingsLayout,
        pane: Pane,
        rows: &[Setting],
        profile: &UserProfile,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let width = work_area.size.width.max(1);
        let height = work_area.size.height.max(1);
        if self.sized_for != (width, height) {
            let mut context = self.buffer.render();
            context.resize((width, height));
            self.sized_for = (width, height);
        }

        {
            let mut context = self.buffer.render();
            let w = width as usize;
            let focus = self.focus;
            let opaque_fallback = self.opaque_fallback;
            let _ = context.draw(|pixels| {
                for px in pixels.chunks_exact_mut(4) {
                    px.copy_from_slice(&[0, 0, 0, 0]);
                }
                paint_settings(pixels, w, layout, pane, rows, profile, focus, opaque_fallback);
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((width, height)),
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

fn paint_settings(
    pixels: &mut [u8],
    width: usize,
    layout: SettingsLayout,
    pane: Pane,
    rows: &[Setting],
    profile: &UserProfile,
    focus: Option<SettingsFocus>,
    opaque_fallback: bool,
) {
    let shadow = Rect::new(
        layout.window.origin.x + 8,
        layout.window.bottom(),
        layout.window.size.width - 16,
        10,
    );
    fill_round_rect(pixels, width, shadow, 5, SHADOW);
    fill_round_rect(
        pixels,
        width,
        layout.window,
        16,
        if opaque_fallback { CARD_OPAQUE } else { CARD },
    );

    // Traffic-light dots in the header, decorative on this shell window.
    for (index, colour) in [LIGHT_RED, LIGHT_AMBER, LIGHT_GREEN].iter().enumerate() {
        fill_circle(
            pixels,
            width,
            layout.window.origin.x + 18 + index as i32 * 18,
            layout.window.origin.y + 18,
            6,
            *colour,
        );
    }

    // Pane title, centred in the header band.
    let title = pane.title();
    let title_w = text_width(title);
    draw_text(
        pixels,
        width,
        Rect::new(
            layout.title.origin.x + (layout.title.size.width - title_w) / 2,
            layout.title.origin.y + 10,
            title_w + 4,
            7,
        ),
        TEXT,
        title,
    );

    // Sidebar: every pane as a row, the active one highlighted.
    fill_rect(pixels, width, layout.sidebar, SIDEBAR);
    for (index, candidate) in Pane::ALL.iter().enumerate() {
        let row = Rect::new(
            layout.sidebar.origin.x + 8,
            layout.sidebar.origin.y + index as i32 * SIDEBAR_ROW,
            layout.sidebar.size.width - 16,
            SIDEBAR_ROW - 4,
        );
        let active = *candidate == pane;
        if active {
            fill_round_rect(pixels, width, row, 8, ACCENT);
        }
        if focus == Some(SettingsFocus::Sidebar(index)) {
            draw_focus_ring(pixels, width, row);
        }
        let label = candidate.title();
        let label_w = text_width(label);
        draw_text(
            pixels,
            width,
            Rect::new(
                row.origin.x + 12,
                row.origin.y + (row.size.height - 7) / 2,
                label_w + 2,
                7,
            ),
            if active { [255, 255, 255, 255] } else { TEXT },
            label,
        );
    }

    // Content rows.
    for (row_index, (row_rect, setting)) in layout.rows.iter().zip(rows).enumerate() {
        if row_rect.bottom() > layout.window.bottom() - 8 {
            break;
        }
        paint_row(
            pixels,
            width,
            *row_rect,
            setting,
            profile,
            focus == Some(SettingsFocus::Row(row_index)),
        );
    }
}

fn paint_row(
    pixels: &mut [u8],
    width: usize,
    rect: Rect,
    setting: &Setting,
    profile: &UserProfile,
    focused: bool,
) {
    if let Setting::Section { title, description } = setting {
        fill_rect(
            pixels,
            width,
            Rect::new(rect.origin.x + 2, rect.origin.y + 8, 3, rect.size.height - 16),
            GAME_ACCENT,
        );
        draw_text(
            pixels,
            width,
            Rect::new(rect.origin.x + 14, rect.origin.y + 6, rect.size.width - 18, 8),
            TEXT,
            title,
        );
        draw_text(
            pixels,
            width,
            Rect::new(rect.origin.x + 14, rect.origin.y + 23, rect.size.width - 18, 8),
            MUTED,
            description,
        );
        return;
    }

    fill_round_rect(pixels, width, rect, 10, ROW);
    if focused {
        draw_focus_ring(pixels, width, rect);
    }

    match setting {
        Setting::Toggle { label, value, .. } => {
            draw_label(pixels, width, rect, label);
            let pill = Rect::new(
                rect.right() - 56,
                rect.origin.y + (rect.size.height - 26) / 2,
                44,
                26,
            );
            let on = *value;
            fill_round_rect(pixels, width, pill, 13, if on { ACCENT } else { KNOB_OFF });
            let knob_x = if on { pill.right() - 13 } else { pill.origin.x + 13 };
            fill_circle(
                pixels,
                width,
                knob_x,
                pill.origin.y + 13,
                11,
                [255, 255, 255, 255],
            );
        }
        Setting::Slider {
            label,
            value,
            min,
            max,
            ..
        } => {
            draw_label(pixels, width, rect, label);
            let span = (max - min).abs().max(1e-6);
            let fraction = ((value - min) / span).clamp(0.0, 1.0);
            let track = Rect::new(
                rect.right() - 190,
                rect.origin.y + rect.size.height / 2 - 3,
                130,
                6,
            );
            fill_round_rect(pixels, width, track, 3, TRACK);
            let fill = Rect::new(
                track.origin.x,
                track.origin.y,
                (track.size.width as f32 * fraction) as i32,
                6,
            );
            if fill.size.width > 0 {
                fill_round_rect(pixels, width, fill, 3, ACCENT);
            }
            fill_circle(
                pixels,
                width,
                track.origin.x + fill.size.width,
                track.origin.y + 3,
                10,
                [255, 255, 255, 255],
            );
            let percent = format!("{}%", (fraction * 100.0).round() as i32);
            let percent_w = text_width(&percent);
            draw_text(
                pixels,
                width,
                Rect::new(track.right() + 8, track.origin.y - 1, percent_w + 2, 7),
                MUTED,
                &percent,
            );
        }
        Setting::Select {
            label,
            options,
            selected,
            ..
        } => {
            draw_label(pixels, width, rect, label);
            let current = options.get(*selected).cloned().unwrap_or_default();
            let text = format!("{current} v");
            let text_w = text_width(&text);
            let box_rect = Rect::new(
                rect.right() - text_w - 36,
                rect.origin.y + (rect.size.height - 24) / 2,
                text_w + 24,
                24,
            );
            fill_round_rect(pixels, width, box_rect, 6, [246, 240, 236, 255]);
            draw_text(
                pixels,
                width,
                Rect::new(box_rect.origin.x + 10, box_rect.origin.y + 8, text_w + 2, 7),
                ACCENT,
                &text,
            );
        }
        Setting::Info { label, value } => {
            draw_label(pixels, width, rect, label);
            let value_w = text_width(value);
            draw_text(
                pixels,
                width,
                Rect::new(
                    rect.right() - value_w - 16,
                    rect.origin.y + (rect.size.height - 7) / 2,
                    value_w + 2,
                    7,
                ),
                MUTED,
                value,
            );
        }
        Setting::Status {
            label,
            value,
            detail,
            tone,
        } => {
            let colour = status_colour(*tone);
            fill_circle(
                pixels,
                width,
                rect.origin.x + 24,
                rect.origin.y + rect.size.height / 2,
                6,
                colour,
            );
            draw_text(
                pixels,
                width,
                Rect::new(rect.origin.x + 40, rect.origin.y + 8, rect.size.width - 52, 8),
                TEXT,
                label,
            );
            draw_text(
                pixels,
                width,
                Rect::new(rect.origin.x + 40, rect.origin.y + 24, rect.size.width - 52, 8),
                colour,
                value,
            );
            let detail_w = text_width(detail);
            draw_text(
                pixels,
                width,
                Rect::new(rect.right() - detail_w - 16, rect.origin.y + 8, detail_w + 2, 8),
                MUTED,
                detail,
            );
        }
        Setting::Action { label, .. } => {
            let label_w = text_width(label);
            let button = Rect::new(
                rect.origin.x + 12,
                rect.origin.y + (rect.size.height - 26) / 2,
                label_w + 24,
                26,
            );
            fill_round_rect(pixels, width, button, 8, ACCENT_SOFT);
            draw_text(
                pixels,
                width,
                Rect::new(button.origin.x + 12, button.origin.y + 9, label_w + 2, 7),
                ACCENT,
                label,
            );
        }
        Setting::UserCard => {
            let avatar = match &profile.avatar {
                crate::user::AvatarSource::Mark(mark) => *mark,
                crate::user::AvatarSource::Image(_) => AvatarMark::Ocean,
            };
            let (outer, inner) = avatar.palette();
            let cx = rect.origin.x + 40;
            let cy = rect.origin.y + rect.size.height / 2;
            fill_circle(pixels, width, cx, cy, 28, outer);
            fill_circle(pixels, width, cx, cy, 18, inner);
            let name_w = text_width(&profile.name);
            draw_text(
                pixels,
                width,
                Rect::new(cx + 40, cy - 8, name_w + 2, 7),
                TEXT,
                &profile.name,
            );
            let edit_w = text_width("Edit");
            draw_text(
                pixels,
                width,
                Rect::new(rect.right() - edit_w - 16, cy - 4, edit_w + 2, 7),
                ACCENT,
                "Edit",
            );
        }
        Setting::Section { .. } => unreachable!("section rows are rendered before interactive rows"),
    }
}

fn status_colour(tone: SettingTone) -> [u8; 4] {
    match tone {
        SettingTone::Neutral => GAME_ACCENT,
        SettingTone::Positive => [35, 139, 81, 255],
        SettingTone::Warning => [174, 104, 21, 255],
        SettingTone::Muted => MUTED,
    }
}

fn draw_focus_ring(pixels: &mut [u8], width: usize, rect: Rect) {
    if rect.size.width < 4 || rect.size.height < 4 {
        return;
    }
    fill_rect(
        pixels,
        width,
        Rect::new(rect.origin.x, rect.origin.y, rect.size.width, 2),
        FOCUS,
    );
    fill_rect(
        pixels,
        width,
        Rect::new(rect.origin.x, rect.bottom() - 2, rect.size.width, 2),
        FOCUS,
    );
    fill_rect(
        pixels,
        width,
        Rect::new(rect.origin.x, rect.origin.y, 2, rect.size.height),
        FOCUS,
    );
    fill_rect(
        pixels,
        width,
        Rect::new(rect.right() - 2, rect.origin.y, 2, rect.size.height),
        FOCUS,
    );
}

fn draw_label(pixels: &mut [u8], width: usize, rect: Rect, label: &str) {
    let label_w = text_width(label);
    draw_text(
        pixels,
        width,
        Rect::new(
            rect.origin.x + 12,
            rect.origin.y + (rect.size.height - 7) / 2,
            label_w + 2,
            7,
        ),
        TEXT,
        label,
    );
}
