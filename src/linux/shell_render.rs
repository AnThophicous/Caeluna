//! The top bar and Control Centre renderer.
//!
//! One strip buffer paints the translucent bar: the white distribution mark
//! where macOS puts its apple, the active application name, the status
//! cluster with control centre glyph, battery, and the clock. A second
//! buffer paints the Control Centre dropdown panel with its tiles and
//! sliders, snapshot from the live system readings.

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

use super::backends::SystemReadings;
use super::pixel::{draw_text, fill_circle, fill_rect, fill_round_rect, text_width};
use crate::{
    control::{Control, ControlLayout, PANEL_RADIUS, SLIDER_HEIGHT},
    topbar::{BAR_HEIGHT, Distribution, TopBarItem, TopBarLayout},
    windowing::{Point, Rect},
};

/// Translucent bar wash over the wallpaper, matching the dock glass.
const BAR: [u8; 4] = [190, 210, 230, 110];
/// The white distribution mark and status glyphs.
const GLYPH: [u8; 4] = [244, 247, 251, 240];
const GLYPH_DIM: [u8; 4] = [222, 232, 240, 200];

const PANEL: [u8; 4] = [230, 238, 247, 235];
const TILE_ON: [u8; 4] = [14, 118, 168, 255];
const TILE_OFF: [u8; 4] = [116, 126, 138, 255];
const TILE_GLYPH_ON: [u8; 4] = [255, 255, 255, 255];
const TILE_GLYPH_OFF: [u8; 4] = [236, 240, 246, 255];
const SLIDER_FILL: [u8; 4] = [255, 255, 255, 235];
const SLIDER_TEXT: [u8; 4] = [22, 30, 42, 255];

/// The active toggle state the control centre shows.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellToggles {
    pub wifi: bool,
    pub bluetooth: bool,
    pub battery_saver: bool,
    pub focus: bool,
    pub dark_mode: bool,
    pub brightness: f32,
    pub volume: f32,
}

/// Everything one top bar frame paints from, snapshotted by the caller.
pub struct BarSnapshot<'a> {
    pub layout: TopBarLayout,
    pub app_name: &'a str,
    pub clock: &'a str,
    pub readings: &'a SystemReadings,
    pub distribution: Distribution,
    /// Unread alerts shown as a small menu-bar badge beside the clock.
    pub unread_notifications: usize,
}

/// Owns the top bar and control centre surfaces.
pub struct ShellRenderer {
    bar: MemoryRenderBuffer,
    bar_sized_for: (i32, i32),
    panel: MemoryRenderBuffer,
    panel_sized_for: (i32, i32),
}

impl ShellRenderer {
    pub fn new() -> Self {
        let make = || {
            MemoryRenderBuffer::new(
                Fourcc::Argb8888,
                (8, 8),
                1,
                smithay::utils::Transform::Normal,
                None,
            )
        };
        Self {
            bar: make(),
            bar_sized_for: (0, 0),
            panel: make(),
            panel_sized_for: (0, 0),
        }
    }

    /// Paint the top bar strip and return its element.
    pub fn bar_element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        snapshot: &BarSnapshot<'_>,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let width = work_area.size.width.max(1);
        if self.bar_sized_for != (width, BAR_HEIGHT) {
            let mut context = self.bar.render();
            context.resize((width, BAR_HEIGHT));
            self.bar_sized_for = (width, BAR_HEIGHT);
        }

        {
            let mut context = self.bar.render();
            let _ = context.draw(|pixels| {
                paint_bar(
                    pixels,
                    width as usize,
                    snapshot.layout,
                    snapshot.app_name,
                    snapshot.clock,
                    snapshot.readings,
                    snapshot.distribution,
                    snapshot.unread_notifications,
                );
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((width, BAR_HEIGHT)),
                )])
            });
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysPoint::from((0.0, 0.0)),
            &self.bar,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }

    /// Paint the Control Centre dropdown and return its element.
    pub fn control_element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        layout: ControlLayout,
        toggles: &ShellToggles,
        readings: &SystemReadings,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let width = work_area.size.width.max(1);
        let height = work_area.size.height.max(1);
        if self.panel_sized_for != (width, height) {
            let mut context = self.panel.render();
            context.resize((width, height));
            self.panel_sized_for = (width, height);
        }

        {
            let mut context = self.panel.render();
            let w = width as usize;
            let _ = context.draw(|pixels| {
                for px in pixels.chunks_exact_mut(4) {
                    px.copy_from_slice(&[0, 0, 0, 0]);
                }
                paint_control(pixels, w, layout, toggles, readings);
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((width, height)),
                )])
            });
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysPoint::from((0.0, 0.0)),
            &self.panel,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

/// Paint the whole bar in one pass.
#[allow(clippy::too_many_arguments)]
fn paint_bar(
    pixels: &mut [u8],
    width: usize,
    layout: TopBarLayout,
    app_name: &str,
    clock: &str,
    readings: &SystemReadings,
    distribution: Distribution,
    unread_notifications: usize,
) {
    fill_rect(pixels, width, layout.bar, BAR);

    // The distribution mark: a white penguin silhouette simplified into a
    // rounded body with the distro badge letter, where the apple would sit.
    let mark_cx = layout.mark.origin.x + layout.mark.size.width / 2;
    let body_top = 5;
    let body = Rect::new(mark_cx - 9, body_top, 18, 16);
    fill_round_rect(pixels, width, body, 7, GLYPH);
    // Head: a smaller rounded square above-left, like the penguin's head.
    fill_round_rect(
        pixels,
        width,
        Rect::new(mark_cx - 6, body_top - 3, 9, 8),
        4,
        GLYPH,
    );
    // The distro letter, dark, centred on the body.
    let letter = distribution.badge();
    let letter_w = text_width(letter);
    draw_text(
        pixels,
        width,
        Rect::new(mark_cx - letter_w / 2, body_top + 5, letter_w + 2, 7),
        [10, 26, 42, 255],
        letter,
    );

    // Active application name.
    let app_w = text_width(app_name);
    draw_text(
        pixels,
        width,
        Rect::new(
            layout.app_name.origin.x + (layout.app_name.size.width - app_w) / 2,
            BAR_HEIGHT / 2 - 3,
            app_w + 2,
            7,
        ),
        GLYPH,
        app_name,
    );

    // Control centre glyph: two slider stubs in a rounded chip.
    let cc = layout.control_centre;
    let cc_cx = cc.origin.x + cc.size.width / 2;
    let cc_cy = cc.origin.y + BAR_HEIGHT / 2;
    fill_round_rect(
        pixels,
        width,
        Rect::new(cc_cx - 12, cc_cy - 8, 24, 16),
        5,
        GLYPH_DIM,
    );
    fill_rect(
        pixels,
        width,
        Rect::new(cc_cx - 8, cc_cy - 4, 16, 2),
        [30, 44, 60, 255],
    );
    fill_rect(
        pixels,
        width,
        Rect::new(cc_cx - 8, cc_cy + 2, 16, 2),
        [30, 44, 60, 255],
    );
    fill_circle(pixels, width, cc_cx - 2, cc_cy - 3, 2, [30, 44, 60, 255]);
    fill_circle(pixels, width, cc_cx + 3, cc_cy + 3, 2, [30, 44, 60, 255]);

    // Battery: outline with a fill proportional to the charge, plus text.
    if let Some(percent) = readings.battery_percent {
        let bat_cx = layout.battery.origin.x + layout.battery.size.width / 2;
        let bat_cy = BAR_HEIGHT / 2;
        let shell = Rect::new(bat_cx - 16, bat_cy - 5, 30, 10);
        fill_round_rect(pixels, width, shell, 3, GLYPH_DIM);
        let inner_w = ((shell.size.width - 4) as f32 * percent / 100.0) as i32;
        let inner = Rect::new(shell.origin.x + 2, shell.origin.y + 2, inner_w.max(0), 6);
        let fill_colour = if readings.battery_charging {
            [46, 204, 113, 255]
        } else if percent < 20.0 {
            [232, 72, 61, 255]
        } else {
            GLYPH
        };
        fill_round_rect(pixels, width, inner, 2, fill_colour);
        fill_rect(
            pixels,
            width,
            Rect::new(shell.right(), bat_cy - 2, 2, 4),
            GLYPH_DIM,
        );

        let label = format!("{}%", percent.round() as i32);
        let label_w = text_width(&label);
        draw_text(
            pixels,
            width,
            Rect::new(shell.origin.x - label_w - 6, bat_cy - 3, label_w + 2, 7),
            GLYPH,
            &label,
        );
    }

    // The clock.
    let clock_w = text_width(clock);
    draw_text(
        pixels,
        width,
        Rect::new(
            layout.clock.origin.x + (layout.clock.size.width - clock_w) / 2,
            BAR_HEIGHT / 2 - 3,
            clock_w + 2,
            7,
        ),
        GLYPH,
        clock,
    );

    // The clock is the notification-centre affordance. Keep the badge small
    // and bounded so a burst of alerts never changes menu-bar geometry.
    if unread_notifications > 0 {
        let label = if unread_notifications > 99 {
            "99+".to_owned()
        } else {
            unread_notifications.to_string()
        };
        let badge_width = text_width(&label) + 8;
        let badge = Rect::new(layout.clock.right() - badge_width, 4, badge_width, 14);
        fill_round_rect(pixels, width, badge, 7, TILE_ON);
        draw_text(
            pixels,
            width,
            Rect::new(badge.origin.x + 4, badge.origin.y + 4, badge_width - 6, 7),
            TILE_GLYPH_ON,
            &label,
        );
    }
}

/// Paint the Control Centre dropdown over a transparent output buffer.
fn paint_control(
    pixels: &mut [u8],
    width: usize,
    layout: ControlLayout,
    toggles: &ShellToggles,
    readings: &SystemReadings,
) {
    // Soft shadow under the panel.
    let shadow = Rect::new(
        layout.panel.origin.x + 8,
        layout.panel.bottom(),
        layout.panel.size.width - 16,
        10,
    );
    fill_round_rect(pixels, width, shadow, 5, [10, 14, 22, 60]);
    fill_round_rect(pixels, width, layout.panel, PANEL_RADIUS, PANEL);

    // Wi-Fi / Bluetooth / Battery Saver round tiles.
    for (rect, control) in [
        (layout.wifi, Control::Wifi),
        (layout.bluetooth, Control::Bluetooth),
        (layout.battery_saver, Control::BatterySaver),
    ] {
        let on = match control {
            Control::Wifi => toggles.wifi,
            Control::Bluetooth => toggles.bluetooth,
            _ => toggles.battery_saver,
        };
        let base = if on { TILE_ON } else { TILE_OFF };
        let cx = rect.origin.x + rect.size.width / 2;
        let cy = rect.origin.y + rect.size.height / 2;

        fill_round_rect(pixels, width, rect, 16, [200, 208, 220, 120]);
        let button = shrink(rect, 11);
        fill_round_rect(pixels, width, button, 14, base);

        let glyph = if on { TILE_GLYPH_ON } else { TILE_GLYPH_OFF };
        let label = match control {
            Control::Wifi => {
                if on {
                    readings.network_name.clone().unwrap_or_else(|| "Wi-Fi".into())
                } else {
                    "Wi-Fi".into()
                }
            }
            Control::Bluetooth => "Bluetooth".into(),
            _ => "Low Power".into(),
        };
        let _ = label;

        // Simple pictograms inside the round button.
        match control {
            Control::Wifi => {
                fill_circle(pixels, width, cx, cy + 6, 2, glyph);
                fill_circle(pixels, width, cx, cy, 4, glyph);
                fill_circle(pixels, width, cx, cy - 6, 6, glyph);
            }
            Control::Bluetooth => {
                fill_rect(pixels, width, Rect::new(cx - 1, cy - 7, 2, 14), glyph);
                fill_circle(pixels, width, cx + 3, cy - 4, 3, glyph);
                fill_circle(pixels, width, cx + 3, cy + 4, 3, glyph);
            }
            _ => {
                let level = readings.battery_percent.unwrap_or(0.0) / 100.0;
                let body = Rect::new(cx - 8, cy - 5, 14, 10);
                fill_round_rect(pixels, width, body, 2, glyph);
                fill_rect(pixels, width, Rect::new(body.right(), cy - 2, 2, 4), glyph);
                let inner_w = ((body.size.width - 4) as f32 * level) as i32;
                if inner_w > 0 {
                    fill_rect(
                        pixels,
                        width,
                        Rect::new(body.origin.x + 2, body.origin.y + 2, inner_w, 6),
                        [30, 44, 60, 255],
                    );
                }
            }
        }
    }

    // The focus pill to the right of the tiles.
    draw_pill(pixels, width, layout.focus, "Focus", toggles.focus);

    // Dark mode row.
    draw_pill(pixels, width, layout.dark_mode, "Dark Mode", toggles.dark_mode);

    // Brightness and volume sliders.
    draw_slider(pixels, width, layout.brightness, toggles.brightness, "Brightness");
    draw_slider(pixels, width, layout.volume, toggles.volume, "Volume");
}

/// A rounded pill with label and on/off state.
fn draw_pill(pixels: &mut [u8], width: usize, rect: Rect, label: &str, on: bool) {
    fill_round_rect(pixels, width, rect, 14, [200, 208, 220, 120]);
    let base = if on { TILE_ON } else { TILE_OFF };
    let glyph = if on { TILE_GLYPH_ON } else { TILE_GLYPH_OFF };

    // The label sits left, the state dot right.
    let label_w = text_width(label);
    draw_text(
        pixels,
        width,
        Rect::new(
            rect.origin.x + 12,
            rect.origin.y + rect.size.height / 2 - 3,
            label_w + 2,
            7,
        ),
        SLIDER_TEXT,
        label,
    );
    fill_circle(
        pixels,
        width,
        rect.right() - 20,
        rect.origin.y + rect.size.height / 2,
        6,
        base,
    );
    let _ = glyph;
}

/// One slider row: track, fill and label.
fn draw_slider(pixels: &mut [u8], width: usize, rect: Rect, value: f32, label: &str) {
    let track = Rect::new(
        rect.origin.x + 8,
        rect.origin.y + SLIDER_HEIGHT / 2 - 7,
        rect.size.width - 16,
        14,
    );
    fill_round_rect(pixels, width, track, 7, TILE_OFF);
    let fill_w = (track.size.width as f32 * value.clamp(0.0, 1.0)) as i32;
    if fill_w > 0 {
        fill_round_rect(
            pixels,
            width,
            Rect::new(track.origin.x, track.origin.y, fill_w, track.size.height),
            7,
            SLIDER_FILL,
        );
    }
    // The label inside the track, left-aligned with padding.
    draw_text(
        pixels,
        width,
        Rect::new(track.origin.x + 8, track.origin.y + 3, track.size.width - 12, 7),
        SLIDER_TEXT,
        label,
    );
}

/// Shrink a rect by `by` on every side.
fn shrink(rect: Rect, by: i32) -> Rect {
    Rect::new(
        rect.origin.x + by,
        rect.origin.y + by,
        (rect.size.width - 2 * by).max(0),
        (rect.size.height - 2 * by).max(0),
    )
}

/// Resolve a bar press when no dropdown is open. Kept for future menus;
/// the nested input path calls the layout directly.
#[allow(dead_code)]
pub fn resolve_press(layout: &TopBarLayout, point: Point) -> TopBarItem {
    crate::topbar::item_at(layout, point)
}
