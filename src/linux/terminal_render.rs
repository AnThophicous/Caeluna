//! Liquid Glass terminal surface for the nested compositor.
//!
//! The terminal is a compositor-owned app surface, not a fake shell. Its
//! visible output is supplied by `terminal_ui::TerminalUi`, which in turn is
//! fed by the real PTY core. This renderer owns one reusable memory buffer,
//! paints only when the UI revision changes, and keeps the glass treatment in
//! the title/tab chrome. Terminal content remains an opaque, high-contrast
//! surface so blur never compromises readability or low-end performance.

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
    utils::Point as PhysicalPoint,
};

use super::{
    fonts::FontBook,
    pixel::{fill_circle, fill_rect, fill_round_rect},
    render_policy::{OutputMetrics, RenderPolicy},
    terminal_ui::{TerminalPalette, TerminalTab, TerminalTheme, TerminalUi},
};
use crate::{
    design::{GlassSurface, PerformanceProfile},
    windowing::Rect,
};

const WINDOW_RADIUS: i32 = 18;
const TITLE_RADIUS: i32 = 14;
const TAB_RADIUS: i32 = 9;
const TEXT_SIZE: f32 = 13.0;
const TAB_SIZE: f32 = 11.0;
const STATUS_SIZE: f32 = 10.0;

const CLOSE_RED: [u8; 4] = [242, 91, 92, 255];
const CLOSE_AMBER: [u8; 4] = [242, 181, 74, 255];
const CLOSE_GREEN: [u8; 4] = [82, 196, 112, 255];
const HAIRLINE: [u8; 4] = [255, 255, 255, 42];

/// Owns one reusable terminal texture. A new PTY line changes the UI
/// revision, but unchanged frames reuse the already-imported buffer.
pub struct TerminalRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
    painted_revision: u64,
}

impl Default for TerminalRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalRenderer {
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
            painted_revision: 0,
        }
    }

    /// Paint the terminal and return a positioned render element.
    pub fn element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        ui: &TerminalUi,
        fonts: &FontBook,
        profile: PerformanceProfile,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        if !ui.open || ui.tabs.is_empty() {
            return None;
        }

        let layout = ui.layout(work_area);
        let width = layout.window.size.width.max(1);
        let height = layout.window.size.height.max(1);
        let resized = self.sized_for != (width, height);
        if resized {
            let mut context = self.buffer.render();
            context.resize((width, height));
            self.sized_for = (width, height);
        }

        if resized || self.painted_revision != ui.revision() {
            self.repaint(width, height, work_area, ui, fonts, profile);
            self.painted_revision = ui.revision();
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysicalPoint::from((layout.window.origin.x as f64, layout.window.origin.y as f64)),
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }

    fn repaint(
        &mut self,
        width: i32,
        height: i32,
        work_area: Rect,
        ui: &TerminalUi,
        fonts: &FontBook,
        profile: PerformanceProfile,
    ) {
        let layout = ui.layout(work_area);
        let Some(tab) = ui.active_tab() else {
            return;
        };
        let palette = tab.theme.palette();
        let policy = RenderPolicy::for_output(
            profile,
            ui.reduce_transparency,
            OutputMetrics {
                width: width.max(1) as u32,
                height: height.max(1) as u32,
                observed_dpr_milli: 1000,
            },
            GlassSurface::GalleryChrome,
            1,
        );
        let chrome = chrome_color(palette, tab, policy);
        let selected_tab = selected_tab_color(palette, tab, policy);

        let mut context = self.buffer.render();
        let _ = context.draw(|pixels| {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[0, 0, 0, 0]);
            }

            let origin = layout.window.origin;
            let local = |rect: Rect| {
                Rect::new(
                    rect.origin.x.saturating_sub(origin.x),
                    rect.origin.y.saturating_sub(origin.y),
                    rect.size.width,
                    rect.size.height,
                )
            };
            let local_window = Rect::new(0, 0, width, height);
            let local_title = local(layout.title_bar);
            let local_tabs = local(layout.tab_bar);
            let local_content = local(layout.content);
            let local_status = local(layout.status_bar);
            let buffer_width = width as usize;

            // One opaque content plane keeps text readable over any wallpaper;
            // the upper chrome is the only place where bounded glass is used.
            fill_round_rect(
                pixels,
                buffer_width,
                local_window,
                WINDOW_RADIUS,
                palette.background,
            );
            fill_rect(pixels, buffer_width, local_content, palette.background);
            fill_round_rect(pixels, buffer_width, local_title, TITLE_RADIUS, chrome);
            fill_rect(pixels, buffer_width, local_tabs, chrome);
            fill_rect(pixels, buffer_width, local_status, palette.status);
            fill_rect(
                pixels,
                buffer_width,
                Rect::new(0, local_content.bottom() - 1, width, 1),
                HAIRLINE,
            );

            draw_traffic_lights(pixels, buffer_width, &local_title);
            draw_title(pixels, buffer_width, fonts, ui, tab, &local_title);
            draw_tabs(pixels, buffer_width, fonts, ui, &layout, palette, selected_tab);
            draw_terminal_content(pixels, buffer_width, fonts, &local_content, tab, palette);
            draw_status(pixels, buffer_width, fonts, &local_status, tab, policy);

            Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                smithay::utils::Size::from((width, height)),
            )])
        });
    }
}

fn chrome_color(palette: TerminalPalette, tab: &TerminalTab, policy: RenderPolicy) -> [u8; 4] {
    let mut color = palette.panel;
    // `reduce_transparency` and the low-end profile make the chrome opaque.
    // The profile decides this without allocating a backdrop per frame.
    if !tab.transparency || policy.material.applied.opaque_content {
        color[3] = 255;
    } else {
        color[3] = color[3].min(224);
    }
    color
}

fn selected_tab_color(palette: TerminalPalette, tab: &TerminalTab, policy: RenderPolicy) -> [u8; 4] {
    let mut color = palette.panel_selected;
    if !tab.transparency || policy.material.applied.opaque_content {
        color[3] = 255;
    }
    color
}

fn draw_traffic_lights(pixels: &mut [u8], width: usize, title: &Rect) {
    let y = title.origin.y + title.size.height / 2;
    for (index, color) in [CLOSE_RED, CLOSE_AMBER, CLOSE_GREEN].into_iter().enumerate() {
        fill_circle(pixels, width, 22 + index as i32 * 20, y, 6, color);
    }
}

fn draw_title(
    pixels: &mut [u8],
    width: usize,
    fonts: &FontBook,
    ui: &TerminalUi,
    tab: &TerminalTab,
    title: &Rect,
) {
    let label = if ui.title_editing {
        ui.title_buffer.as_str()
    } else {
        tab.title.as_str()
    };
    let label = ellipsize(label, 28);
    let label_width = fonts.text_width(&label, TEXT_SIZE);
    let x = (title.size.width - label_width) / 2;
    let color = if ui.focused {
        palette_text(tab.theme)
    } else {
        palette_muted(tab.theme)
    };
    draw_text(
        pixels,
        width,
        Rect::new(x.max(116), title.origin.y + 14, 300, 18),
        color,
        &label,
        fonts,
        TEXT_SIZE,
    );
    // A small caret makes the edit state visible without an always-running
    // blink animation (the next input event schedules repaint).
    if ui.title_editing {
        let caret_x = (x + label_width + 2).min(title.right() - 8);
        fill_rect(
            pixels,
            width,
            Rect::new(caret_x, title.origin.y + 12, 1, 18),
            palette_text(tab.theme),
        );
    }
}

fn draw_tabs(
    pixels: &mut [u8],
    width: usize,
    fonts: &FontBook,
    ui: &TerminalUi,
    layout: &super::terminal_ui::TerminalLayout,
    palette: TerminalPalette,
    selected: [u8; 4],
) {
    for (index, geometry) in layout.tabs.iter().enumerate() {
        let Some(tab) = ui.tabs.get(index) else {
            continue;
        };
        let rect = local_rect(geometry.tab, layout.window.origin);
        let fill = if index == ui.active_tab {
            selected
        } else {
            palette.panel
        };
        fill_round_rect(pixels, width, rect, TAB_RADIUS, fill);
        let title = ellipsize(&tab.title, 18);
        let title_color = if index == ui.active_tab {
            palette.text
        } else {
            palette.muted
        };
        draw_text(
            pixels,
            width,
            Rect::new(rect.origin.x + 12, rect.origin.y + 9, rect.size.width - 36, 16),
            title_color,
            &title,
            fonts,
            TAB_SIZE,
        );
        draw_text(
            pixels,
            width,
            local_rect(geometry.close, layout.window.origin),
            palette.muted,
            "x",
            fonts,
            TAB_SIZE,
        );
    }

    let new_tab = local_rect(layout.new_tab, layout.window.origin);
    let theme = local_rect(layout.theme, layout.window.origin);
    let transparency = local_rect(layout.transparency, layout.window.origin);
    let blur = local_rect(layout.blur, layout.window.origin);
    draw_control(pixels, width, fonts, new_tab, palette.accent, "+");
    draw_control(pixels, width, fonts, theme, palette.muted, "A");
    draw_control(pixels, width, fonts, transparency, palette.muted, "T");
    draw_control(pixels, width, fonts, blur, palette.muted, "G");
}

fn draw_control(pixels: &mut [u8], width: usize, fonts: &FontBook, rect: Rect, color: [u8; 4], label: &str) {
    fill_round_rect(pixels, width, rect, 7, [255, 255, 255, 22]);
    draw_text(
        pixels,
        width,
        Rect::new(rect.origin.x + 8, rect.origin.y + 6, rect.size.width - 10, 14),
        color,
        label,
        fonts,
        TAB_SIZE,
    );
}

fn draw_terminal_content(
    pixels: &mut [u8],
    width: usize,
    fonts: &FontBook,
    content: &Rect,
    tab: &TerminalTab,
    palette: TerminalPalette,
) {
    let x = content.origin.x + 20;
    let top = content.origin.y + 18;
    let line_height = 20;
    let visible = ((content.size.height - 28).max(0) / line_height) as usize;
    let start = tab.lines.len().saturating_sub(visible);

    if tab.lines.is_empty() {
        draw_text(
            pixels,
            width,
            Rect::new(x, top, content.size.width - 32, 18),
            palette.muted,
            "Waiting for the real shell session...",
            fonts,
            TEXT_SIZE,
        );
    } else {
        for (row, line) in tab.lines[start..].iter().enumerate() {
            let y = top + row as i32 * line_height;
            if y + line_height > content.bottom() - 8 {
                break;
            }
            draw_text(
                pixels,
                width,
                Rect::new(x, y, content.size.width - 32, line_height),
                palette.text,
                line,
                fonts,
                TEXT_SIZE,
            );
        }
    }

    if tab.alive && tab.cursor_visible {
        let row = tab
            .cursor_row
            .saturating_sub(start)
            .min(visible.saturating_sub(1));
        let line = tab.lines.get(tab.cursor_row).map(String::as_str).unwrap_or("");
        let prefix: String = line.chars().take(tab.cursor_column).collect();
        let cursor_x = x + fonts.text_width(&prefix, TEXT_SIZE);
        let cursor_y = top + row as i32 * line_height + 2;
        if content.contains_point(Point {
            x: cursor_x,
            y: cursor_y,
        }) {
            fill_rect(
                pixels,
                width,
                Rect::new(cursor_x, cursor_y, 2, 16),
                palette.cursor,
            );
        }
    }
}

fn draw_status(
    pixels: &mut [u8],
    width: usize,
    fonts: &FontBook,
    status: &Rect,
    tab: &TerminalTab,
    policy: RenderPolicy,
) {
    let material = if policy.material.applied.opaque_content {
        "Opaque"
    } else if tab.blur && policy.material.applied.blur_radius_px > 0 {
        // The current memory-buffer path intentionally does not sample the
        // backdrop; this label describes the bounded material policy rather
        // than claiming that a per-frame blur pass was allocated.
        "Glass"
    } else {
        "Tint"
    };
    let state = if tab.alive {
        "UTF-8  Ready"
    } else {
        "UTF-8  Exited"
    };
    let label = format!("{}   {}   {}   {}", tab.cwd, state, tab.theme.label(), material);
    draw_text(
        pixels,
        width,
        Rect::new(
            status.origin.x + 18,
            status.origin.y + 7,
            status.size.width - 32,
            14,
        ),
        tab.theme.palette().muted,
        &label,
        fonts,
        STATUS_SIZE,
    );
}

fn local_rect(rect: Rect, origin: crate::windowing::Point) -> Rect {
    Rect::new(
        rect.origin.x.saturating_sub(origin.x),
        rect.origin.y.saturating_sub(origin.y),
        rect.size.width,
        rect.size.height,
    )
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    let mut value = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        value.pop();
        value.push('…');
    }
    value
}

fn palette_text(theme: TerminalTheme) -> [u8; 4] {
    theme.palette().text
}

fn palette_muted(theme: TerminalTheme) -> [u8; 4] {
    theme.palette().muted
}

// Keep the text helper call sites visually explicit: this wrapper documents
// that all terminal labels use the shared FontBook and never a new font path.
fn draw_text(
    pixels: &mut [u8],
    width: usize,
    rect: Rect,
    color: [u8; 4],
    text: &str,
    fonts: &FontBook,
    size: f32,
) {
    fonts.draw_text(pixels, width, rect, color, text, size);
}

use crate::windowing::Point;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsize_preserves_short_titles_and_caps_long_titles() {
        assert_eq!(ellipsize("Terminal", 8), "Terminal");
        assert_eq!(ellipsize("123456789", 8), "1234567…");
    }

    #[test]
    fn low_end_material_is_opaque_without_backdrop_sampling() {
        let palette = TerminalTheme::Ocean.palette();
        let mut tab = TerminalTab::new(1);
        tab.transparency = true;
        let policy = RenderPolicy::for_output(
            PerformanceProfile::LowEnd,
            false,
            OutputMetrics {
                width: 1920,
                height: 1080,
                observed_dpr_milli: 1000,
            },
            GlassSurface::GalleryChrome,
            1,
        );
        assert_eq!(
            policy.material.applied.backdrop,
            crate::design::BackdropQuality::Disabled
        );
        assert_eq!(chrome_color(palette, &tab, policy)[3], 255);
    }

    #[test]
    fn selected_tab_keeps_alpha_only_when_policy_allows_it() {
        let palette = TerminalTheme::Ocean.palette();
        let tab = TerminalTab::new(1);
        let policy = RenderPolicy::for_output(
            PerformanceProfile::Balanced,
            false,
            OutputMetrics {
                width: 1280,
                height: 800,
                observed_dpr_milli: 1000,
            },
            GlassSurface::GalleryChrome,
            1,
        );
        assert!(selected_tab_color(palette, &tab, policy)[3] < 255);
    }
}
