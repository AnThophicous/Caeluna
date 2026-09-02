//! Native renderer for the Rouch Flatpak application gallery.
//!
//! The gallery is intentionally rendered into one bounded memory buffer. It
//! uses the same pixel/font primitives as the existing shell surfaces, so it
//! does not add a second UI toolkit or animation engine to the compositor.

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

use super::{
    fonts::FontBook,
    pixel::{draw_text, fill_rect, fill_round_rect, text_width},
};
use crate::{
    app_store::{AppCategory, AppStore, CatalogState, FlatpakApp},
    windowing::Rect,
};

const WINDOW: [u8; 4] = [244, 248, 252, 250];
const TOOLBAR: [u8; 4] = [234, 241, 248, 250];
const SIDEBAR: [u8; 4] = [225, 235, 244, 235];
const CONTENT: [u8; 4] = [249, 251, 253, 245];
const CARD: [u8; 4] = [255, 255, 255, 255];
const CARD_SELECTED: [u8; 4] = [220, 243, 252, 255];
const TEXT: [u8; 4] = [22, 34, 47, 255];
const MUTED: [u8; 4] = [92, 108, 123, 255];
const SUBTLE: [u8; 4] = [142, 157, 170, 255];
const ACCENT: [u8; 4] = [8, 127, 178, 255];
const ACCENT_SOFT: [u8; 4] = [207, 237, 248, 255];
const ERROR: [u8; 4] = [188, 55, 62, 255];
const WARNING: [u8; 4] = [165, 106, 22, 255];

/// Owns the reusable gallery buffer.
pub struct AppStoreRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
}

impl Default for AppStoreRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl AppStoreRenderer {
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

    /// Paint the current gallery state and return a positioned render element.
    pub fn element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        store: &AppStore,
        fonts: &FontBook,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let layout = store.layout(work_area);
        let width = layout.window.size.width.max(1);
        let height = layout.window.size.height.max(1);
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
                let origin = layout.window.origin;
                let local = |rect: Rect| {
                    Rect::new(
                        rect.origin.x - origin.x,
                        rect.origin.y - origin.y,
                        rect.size.width,
                        rect.size.height,
                    )
                };
                let w = width as usize;

                fill_round_rect(pixels, w, Rect::new(0, 0, width, height), 18, WINDOW);
                fill_rect(pixels, w, local(layout.toolbar), TOOLBAR);
                fill_rect(pixels, w, local(layout.sidebar), SIDEBAR);
                fill_rect(pixels, w, local(layout.content), CONTENT);

                draw_text(pixels, w, Rect::new(22, 18, 300, 14), TEXT, "App Gallery");
                draw_text(
                    pixels,
                    w,
                    Rect::new(22, 39, 330, 10),
                    MUTED,
                    "Install and launch software from Flatpak Hub",
                );

                let search = local(layout.search);
                fill_round_rect(pixels, w, search, 10, [215, 225, 235, 255]);
                let search_label = if store.query.is_empty() {
                    "Search apps"
                } else {
                    &store.query
                };
                let search_color = if store.query.is_empty() { SUBTLE } else { TEXT };
                draw_text(
                    pixels,
                    w,
                    Rect::new(
                        search.origin.x + 12,
                        search.origin.y + 10,
                        search.size.width - 18,
                        10,
                    ),
                    search_color,
                    search_label,
                );

                for (index, category) in AppCategory::ALL.iter().enumerate() {
                    let rect = local(layout.categories[index]);
                    let selected = *category == store.category;
                    if selected {
                        fill_round_rect(pixels, w, rect, 9, ACCENT_SOFT);
                    }
                    draw_text(
                        pixels,
                        w,
                        Rect::new(rect.origin.x + 10, rect.origin.y + 12, rect.size.width - 16, 9),
                        if selected { ACCENT } else { TEXT },
                        category.label(),
                    );
                }

                let visible = store.filtered();
                for (index, app) in visible.iter().enumerate() {
                    let rect = local(layout.cards[index]);
                    let selected = store.selected == Some(index);
                    fill_round_rect(pixels, w, rect, 12, if selected { CARD_SELECTED } else { CARD });
                    draw_app_card(pixels, w, rect, app, fonts);
                }

                if visible.is_empty() {
                    let (title, detail, colour) = match &store.state {
                        CatalogState::Loading => (
                            "Loading Flatpak Hub",
                            "Checking the configured Flathub remote...",
                            MUTED,
                        ),
                        CatalogState::Offline => (
                            "Flatpak is offline",
                            "Cached or locally installed apps remain available.",
                            WARNING,
                        ),
                        CatalogState::Error(message) => {
                            ("Could not load the catalogue", message.as_str(), ERROR)
                        }
                        CatalogState::Empty if store.shows_no_results() => (
                            "No apps match this search",
                            "Try an app name, category, or Flatpak ID.",
                            MUTED,
                        ),
                        CatalogState::Empty | CatalogState::Ready => (
                            "No applications available",
                            "Connect Flatpak to a remote or refresh the catalogue.",
                            MUTED,
                        ),
                    };
                    let centre_x = layout.content.origin.x - origin.x + layout.content.size.width / 2;
                    draw_centered(
                        pixels,
                        w,
                        centre_x,
                        layout.content.origin.y - origin.y + 180,
                        title,
                        fonts,
                        colour,
                        16.0,
                    );
                    draw_centered(
                        pixels,
                        w,
                        centre_x,
                        layout.content.origin.y - origin.y + 210,
                        detail,
                        fonts,
                        MUTED,
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
            PhysPoint::from((layout.window.origin.x as f64, layout.window.origin.y as f64)),
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

fn draw_app_card(pixels: &mut [u8], width: usize, rect: Rect, app: &FlatpakApp, fonts: &FontBook) {
    let icon = Rect::new(rect.origin.x + 14, rect.origin.y + 14, 58, 58);
    fill_round_rect(pixels, width, icon, 15, icon_tint(app.app_id()));
    let initial = app
        .name()
        .chars()
        .next()
        .unwrap_or('?')
        .to_ascii_uppercase()
        .to_string();
    let initial_w = fonts.text_width(&initial, 25.0);
    fonts.draw_text(
        pixels,
        width,
        Rect::new(
            icon.origin.x + (icon.size.width - initial_w) / 2,
            icon.origin.y + 16,
            initial_w + 4,
            28,
        ),
        [255, 255, 255, 250],
        &initial,
        25.0,
    );

    draw_text(
        pixels,
        width,
        Rect::new(rect.origin.x + 14, rect.origin.y + 84, rect.size.width - 28, 10),
        TEXT,
        &truncate(app.name(), 22),
    );
    draw_text(
        pixels,
        width,
        Rect::new(rect.origin.x + 14, rect.origin.y + 103, rect.size.width - 28, 18),
        MUTED,
        &truncate(app.summary(), 29),
    );

    let origin = app.metadata.origin.as_deref().unwrap_or("Local");
    draw_text(
        pixels,
        width,
        Rect::new(rect.origin.x + 14, rect.origin.y + 132, rect.size.width - 28, 9),
        SUBTLE,
        &truncate(origin, 24),
    );
    let action = match app.primary_action() {
        crate::app_store::AppAction::Install => "Install",
        crate::app_store::AppAction::Update => "Update",
        crate::app_store::AppAction::Open => "Open",
    };
    let action_w = text_width(action);
    let action_rect = Rect::new(
        rect.origin.x + 14,
        rect.bottom() - 38,
        (action_w + 24).min(rect.size.width - 28),
        24,
    );
    fill_round_rect(pixels, width, action_rect, 8, ACCENT_SOFT);
    draw_text(
        pixels,
        width,
        Rect::new(
            action_rect.origin.x + 12,
            action_rect.origin.y + 9,
            action_rect.size.width - 18,
            9,
        ),
        ACCENT,
        action,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_centered(
    pixels: &mut [u8],
    width: usize,
    centre_x: i32,
    y: i32,
    text: &str,
    fonts: &FontBook,
    colour: [u8; 4],
    size: f32,
) {
    let text_width = fonts.text_width(text, size);
    fonts.draw_text(
        pixels,
        width,
        Rect::new(
            centre_x - text_width / 2,
            y,
            text_width + 4,
            size.ceil() as i32 + 4,
        ),
        colour,
        text,
        size,
    );
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.pop();
        output.push('…');
    }
    output
}

fn icon_tint(id: &str) -> [u8; 4] {
    let hash = id.bytes().fold(17u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    });
    let palette = [
        [8, 127, 178, 255],
        [61, 105, 186, 255],
        [77, 145, 115, 255],
        [174, 105, 54, 255],
        [122, 94, 166, 255],
    ];
    palette[(hash as usize) % palette.len()]
}
