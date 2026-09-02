//! The Finder window renderer: glass window, sidebar of places, toolbar
//! with back/forward and search, and the file grid, macOS style.

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
use crate::finder::{FinderItem, FinderState, LABEL, TILE};
use crate::windowing::Rect;

const CARD: [u8; 4] = [246, 252, 240, 248];
const SIDEBAR: [u8; 4] = [242, 234, 228, 255];
const TEXT: [u8; 4] = [42, 30, 22, 255];
const MUTED: [u8; 4] = [112, 100, 90, 255];
const BUTTON: [u8; 4] = [216, 210, 204, 255];
const BUTTON_OFF: [u8; 4] = [240, 238, 236, 255];
const SEARCH: [u8; 4] = [228, 222, 216, 255];
const SHADOW: [u8; 4] = [60, 22, 14, 60];
const SELECTION: [u8; 4] = [168, 118, 14, 96];

const FOLDER_BACK: [u8; 4] = [166, 230, 88, 255];
const FOLDER_FRONT: [u8; 4] = [120, 198, 244, 255];
const PAGE: [u8; 4] = [252, 252, 252, 255];
const PAGE_EAR: [u8; 4] = [214, 206, 200, 255];

/// Owns the Finder window surface.
pub struct FinderRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
}

impl Default for FinderRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl FinderRenderer {
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

    /// Paint the Finder window over a transparent output buffer.
    pub fn element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        state: &FinderState,
        items: &[FinderItem],
        places: &[(String, String)],
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
            let _ = context.draw(|pixels| {
                for px in pixels.chunks_exact_mut(4) {
                    px.copy_from_slice(&[0, 0, 0, 0]);
                }
                paint_finder(pixels, w, work_area, state, items, places);
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

fn paint_finder(
    pixels: &mut [u8],
    width: usize,
    work_area: Rect,
    state: &FinderState,
    items: &[FinderItem],
    places: &[(String, String)],
) {
    let window = crate::finder::window_rect(work_area);
    let (sidebar, toolbar, _content, grid) = crate::finder::layout(window);

    let shadow = Rect::new(window.origin.x + 8, window.bottom(), window.size.width - 16, 10);
    fill_round_rect(pixels, width, shadow, 5, SHADOW);
    fill_round_rect(pixels, width, window, 16, CARD);
    fill_rect(pixels, width, sidebar, SIDEBAR);

    paint_toolbar(pixels, width, toolbar, window, state);
    paint_sidebar(pixels, width, sidebar, places, state);

    // Grid tiles.
    for (index, item) in items.iter().enumerate() {
        let rect = crate::finder::item_rect(grid, index);
        if rect.bottom() > window.bottom() - 8 {
            break;
        }
        let selected = state.selection == Some(index);
        if selected {
            fill_round_rect(pixels, width, rect, 12, SELECTION);
        }
        paint_item(pixels, width, rect, item);
    }
}

fn paint_toolbar(pixels: &mut [u8], width: usize, toolbar: Rect, window: Rect, state: &FinderState) {
    let can_back = state.history.len() > 1;
    let can_forward = !state.forward.is_empty();
    let cy = toolbar.origin.y + toolbar.size.height / 2;

    // Back / forward buttons.
    for (offset, enabled, glyph) in [(0, can_back, "<"), (1, can_forward, ">")] {
        let cx = window.origin.x + 34 + offset * 40;
        let colour = if enabled { BUTTON } else { BUTTON_OFF };
        fill_circle(pixels, width, cx, cy, 14, colour);
        let glyph_w = text_width(glyph);
        draw_text(
            pixels,
            width,
            Rect::new(cx - glyph_w / 2, cy - 3, glyph_w + 2, 7),
            if enabled { [255, 255, 255, 255] } else { MUTED },
            glyph,
        );
    }

    // The current folder's name, centred in the toolbar.
    let title = state.current().rsplit('/').next().unwrap_or("/");
    let title_w = text_width(title);
    draw_text(
        pixels,
        width,
        Rect::new(
            toolbar.origin.x + (toolbar.size.width - title_w) / 2,
            cy - 3,
            title_w + 2,
            7,
        ),
        TEXT,
        title,
    );

    // Search field.
    let search = Rect::new(toolbar.right() - 190, cy - 13, 170, 26);
    fill_round_rect(pixels, width, search, 8, SEARCH);
    let query_w = text_width(&state.query);
    draw_text(
        pixels,
        width,
        Rect::new(search.origin.x + 10, cy - 3, query_w + 2, 7),
        TEXT,
        &state.query,
    );
    if state.query.is_empty() {
        let hint_w = text_width("Search");
        draw_text(
            pixels,
            width,
            Rect::new(search.origin.x + 10, cy - 3, hint_w + 2, 7),
            MUTED,
            "Search",
        );
    }
}

fn paint_sidebar(
    pixels: &mut [u8],
    width: usize,
    sidebar: Rect,
    places: &[(String, String)],
    state: &FinderState,
) {
    let row_height = 34;
    for (index, (label, path)) in places.iter().enumerate() {
        let row = Rect::new(
            sidebar.origin.x + 8,
            sidebar.origin.y + index as i32 * row_height,
            sidebar.size.width - 16,
            row_height - 6,
        );
        let active = path == state.current();
        if active {
            fill_round_rect(pixels, width, row, 8, SELECTION);
        }
        fill_round_rect(
            pixels,
            width,
            Rect::new(row.origin.x, row.origin.y + 6, 14, 14),
            4,
            tint(label),
        );
        let label_w = text_width(label);
        draw_text(
            pixels,
            width,
            Rect::new(
                row.origin.x + 24,
                row.origin.y + (row.size.height - 7) / 2,
                label_w + 2,
                7,
            ),
            TEXT,
            label,
        );
    }
}

fn paint_item(pixels: &mut [u8], width: usize, rect: Rect, item: &FinderItem) {
    let icon = Rect::new(
        rect.origin.x + (rect.size.width - TILE) / 2,
        rect.origin.y,
        TILE,
        TILE,
    );

    if item.directory {
        // Folder: a tab and a body.
        fill_round_rect(
            pixels,
            width,
            Rect::new(icon.origin.x + 4, icon.origin.y + 12, 28, 10),
            3,
            FOLDER_BACK,
        );
        fill_round_rect(
            pixels,
            width,
            Rect::new(icon.origin.x + 4, icon.origin.y + 18, TILE - 8, TILE - 26),
            6,
            FOLDER_BACK,
        );
        fill_round_rect(
            pixels,
            width,
            Rect::new(icon.origin.x + 6, icon.origin.y + 20, TILE - 12, TILE - 30),
            5,
            FOLDER_FRONT,
        );
    } else if item.name.ends_with(".desktop") {
        fill_round_rect(pixels, width, icon, icon.size.width / 5, tint(&item.name));
    } else {
        // A page with a dog-ear.
        fill_rect(
            pixels,
            width,
            Rect::new(icon.origin.x + 16, icon.origin.y + 8, TILE - 32, TILE - 16),
            PAGE,
        );
        fill_rect(
            pixels,
            width,
            Rect::new(icon.right() - 28, icon.origin.y + 8, 12, 12),
            PAGE_EAR,
        );
    }

    // Label, up to two lines.
    let label_rect = Rect::new(rect.origin.x + 2, icon.bottom() + 2, rect.size.width - 4, LABEL);
    let mid = (item.name.len() / 2).clamp(1, item.name.len().saturating_sub(1));
    let first = &item.name[..mid.min(item.name.len())];
    let second = &item.name[mid.min(item.name.len())..];
    draw_text(pixels, width, label_rect, TEXT, first);
    let second_rect = Rect::new(
        label_rect.origin.x,
        label_rect.origin.y + 9,
        label_rect.size.width,
        7,
    );
    draw_text(pixels, width, second_rect, TEXT, second.trim());
}

/// A stable colour for a name, from the ocean palette.
fn tint(name: &str) -> [u8; 4] {
    let hash = name
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
