//! The welcome experience renderer: first-run film and update card.
//!
//! Both surfaces are full-output `MemoryRenderBuffer`s painted from the pure
//! timelines in [`crate::welcome`]. The first-run film composites over the
//! live desktop (it fades out into it); the update card floats above it.

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
    pixel::{draw_text, fill_circle, fill_rect, fill_round_rect, text_width},
};
use crate::welcome::{
    ReleaseNotes, TutorialFocus, TutorialLayout, TutorialState, TutorialStep, TutorialTarget, UpdateFrame,
    WelcomeFrame, first_run_layout, tutorial_layout, update_layout,
};
use crate::windowing::Rect;

/// Deep ocean backdrop for the first-run film, from the wallpaper palette.
const OCEAN: [u8; 4] = [8, 28, 52, 255];
const MARK_FACE: [u8; 4] = [224, 244, 250, 255];
const MARK_CORE: [u8; 4] = [16, 130, 170, 255];
const WORDMARK: [u8; 4] = [232, 242, 250, 255];

const CARD: [u8; 4] = [242, 246, 250, 250];
const CARD_TITLE: [u8; 4] = [22, 30, 42, 255];
const CARD_TEXT: [u8; 4] = [52, 64, 80, 255];
const CARD_BUTTON: [u8; 4] = [14, 118, 168, 255];
const CARD_BUTTON_TEXT: [u8; 4] = [255, 255, 255, 255];
const CARD_ACCENT: [u8; 4] = [86, 164, 205, 255];

// The tutorial uses one bounded translucent sheet. It does not blur or sample
// the wallpaper every frame: the glass read is supplied by a tonal fill,
// highlight edge and a small amount of depth, with an authored opaque mode
// for reduced transparency and low-end hardware.
const TUTORIAL_BACKDROP_GLASS: [u8; 4] = [5, 17, 35, 178];
const TUTORIAL_BACKDROP_OPAQUE: [u8; 4] = [5, 17, 35, 242];
const TUTORIAL_CARD_GLASS: [u8; 4] = [236, 247, 253, 236];
const TUTORIAL_CARD_OPAQUE: [u8; 4] = [239, 247, 252, 255];
const TUTORIAL_SIDEBAR_GLASS: [u8; 4] = [209, 232, 243, 164];
const TUTORIAL_SIDEBAR_OPAQUE: [u8; 4] = [218, 237, 246, 255];
const TUTORIAL_TEXT: [u8; 4] = [18, 35, 52, 255];
const TUTORIAL_MUTED: [u8; 4] = [70, 93, 111, 255];
const TUTORIAL_ACCENT: [u8; 4] = [8, 124, 177, 255];
const TUTORIAL_ACCENT_SOFT: [u8; 4] = [203, 233, 245, 255];
const TUTORIAL_BUTTON: [u8; 4] = [220, 237, 246, 255];
const TUTORIAL_SHADOW: [u8; 4] = [0, 10, 26, 72];
const TUTORIAL_FOCUS: [u8; 4] = [8, 105, 165, 255];

/// Material choice for the tutorial surface. The fallback remains a designed
/// surface instead of merely removing alpha from the Liquid Glass version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialVisualMode {
    LiquidGlass,
    Opaque,
}

impl TutorialVisualMode {
    /// Select the cheap, contrast-stable treatment when either system policy
    /// or a low-end profile asks for it.
    pub const fn for_constraints(low_end: bool, reduce_transparency: bool) -> Self {
        if low_end || reduce_transparency {
            Self::Opaque
        } else {
            Self::LiquidGlass
        }
    }
}

/// Renderer-facing snapshot. Keeping it separate from `TutorialState` lets
/// the session layer choose a capability-aware material without coupling the
/// pure onboarding model to Linux or Smithay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TutorialRenderModel {
    pub step: TutorialStep,
    pub focus: TutorialFocus,
    pub visual_mode: TutorialVisualMode,
    pub reduced_motion: bool,
}

impl TutorialRenderModel {
    pub const fn from_state(state: TutorialState, visual_mode: TutorialVisualMode) -> Option<Self> {
        if !state.is_open() {
            return None;
        }
        Some(Self {
            step: state.step,
            focus: state.focus,
            visual_mode,
            reduced_motion: state.reduced_motion,
        })
    }
}

/// Build a render snapshot for the next compositor frame.
pub const fn tutorial_render_model(
    state: TutorialState,
    visual_mode: TutorialVisualMode,
) -> Option<TutorialRenderModel> {
    TutorialRenderModel::from_state(state, visual_mode)
}

/// Owns the welcome surface.
pub struct WelcomeRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
}

impl WelcomeRenderer {
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

    /// Paint one frame of the first-run film and return its element.
    pub fn first_run_element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        frame: WelcomeFrame,
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
                paint_first_run(pixels, w, work_area, frame);
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((self.sized_for.0, self.sized_for.1)),
                )])
            });
        }

        let alpha = if frame.finished {
            0.0
        } else {
            1.0 - frame.desktop_progress
        };
        if alpha <= 0.0 {
            return None;
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysPoint::from((0.0, 0.0)),
            &self.buffer,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }

    /// Paint one frame of the update card and return its element.
    pub fn update_element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        frame: UpdateFrame,
        notes: &ReleaseNotes,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        if frame.card_progress >= 1.0 && frame.finished {
            // Settled cards stay visible until dismissed; nothing special.
        }
        self.ensure_size(work_area.size.width, work_area.size.height);
        {
            let mut context = self.buffer.render();
            let w = self.sized_for.0 as usize;
            let _ = context.draw(|pixels| {
                paint_update(pixels, w, work_area, frame, notes);
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

    /// Paint the resumable first-steps tutorial and return its output-sized
    /// element. The caller owns `TutorialState`; this method only consumes a
    /// bounded render snapshot and a font book loaded once at session start.
    ///
    /// Rectangles exposed by [`crate::welcome::tutorial_layout`] are absolute
    /// work-area coordinates. The raster buffer is local, so the paint pass
    /// deliberately lays out a local copy; the element is then positioned at
    /// the work-area origin for multi-output correctness.
    pub fn tutorial_element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        model: TutorialRenderModel,
        fonts: &FontBook,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let width = work_area.size.width.max(1);
        let height = work_area.size.height.max(1);
        self.ensure_size(width, height);
        {
            let mut context = self.buffer.render();
            let local_area = Rect::new(0, 0, width, height);
            let layout = tutorial_layout(local_area);
            let _ = context.draw(|pixels| {
                paint_tutorial(pixels, width as usize, layout, model, fonts);
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((width, height)),
                )])
            });
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysPoint::from((work_area.origin.x as f64, work_area.origin.y as f64)),
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

fn paint_tutorial(
    pixels: &mut [u8],
    width: usize,
    layout: TutorialLayout,
    model: TutorialRenderModel,
    fonts: &FontBook,
) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[0, 0, 0, 0]);
    }

    let (backdrop, card, sidebar) = match model.visual_mode {
        TutorialVisualMode::LiquidGlass => (
            TUTORIAL_BACKDROP_GLASS,
            TUTORIAL_CARD_GLASS,
            TUTORIAL_SIDEBAR_GLASS,
        ),
        TutorialVisualMode::Opaque => (
            TUTORIAL_BACKDROP_OPAQUE,
            TUTORIAL_CARD_OPAQUE,
            TUTORIAL_SIDEBAR_OPAQUE,
        ),
    };
    fill_rect(pixels, width, layout.backdrop, backdrop);

    let shadow = Rect::new(
        layout.card.origin.x + 8,
        layout.card.bottom().saturating_sub(2),
        layout.card.size.width.saturating_sub(16),
        10,
    );
    fill_round_rect(pixels, width, shadow, 5, TUTORIAL_SHADOW);
    fill_round_rect(pixels, width, layout.card, 22, card);

    // One narrow highlight line gives the sheet a glass edge without a
    // per-frame blur or an unbounded filtered region.
    let highlight = Rect::new(
        layout.card.origin.x + 22,
        layout.card.origin.y + 1,
        layout.card.size.width.saturating_sub(44),
        2,
    );
    fill_round_rect(pixels, width, highlight, 1, [255, 255, 255, 110]);

    // A stable left rail is the tutorial's visual anchor. On narrow outputs
    // it becomes a compact illustration band through the responsive layout.
    fill_round_rect(pixels, width, layout.illustration, 18, sidebar);
    paint_illustration(pixels, width, layout.illustration, model.step.content().target);

    let track = layout.progress;
    fill_round_rect(pixels, width, track, 2, [155, 188, 204, 190]);
    let fill_width = ((i64::from(track.size.width.max(0)) * model.step.number() as i64) / 6)
        .clamp(0, i64::from(track.size.width.max(0))) as i32;
    if fill_width > 0 {
        fill_round_rect(
            pixels,
            width,
            Rect::new(track.origin.x, track.origin.y, fill_width, track.size.height),
            2,
            TUTORIAL_ACCENT,
        );
    }

    let content = model.step.content();
    fonts.draw_text(
        pixels,
        width,
        layout.eyebrow,
        TUTORIAL_ACCENT,
        step_eyebrow(model.step),
        10.0,
    );
    fonts.draw_text(pixels, width, layout.title, TUTORIAL_TEXT, content.title, 22.0);
    for (index, line) in content.body.iter().take(3).enumerate() {
        let y = layout.body.origin.y.saturating_add(index as i32 * 19);
        fonts.draw_text(
            pixels,
            width,
            Rect::new(layout.body.origin.x, y, layout.body.size.width, 16),
            TUTORIAL_MUTED,
            line,
            12.0,
        );
    }

    fill_round_rect(pixels, width, layout.shortcut, 10, TUTORIAL_ACCENT_SOFT);
    fonts.draw_text(
        pixels,
        width,
        Rect::new(
            layout.shortcut.origin.x + 12,
            layout.shortcut.origin.y + 8,
            layout.shortcut.size.width.saturating_sub(24),
            14,
        ),
        TUTORIAL_ACCENT,
        content.shortcut,
        10.0,
    );

    paint_close_button(
        pixels,
        width,
        layout.close_button,
        model.focus == TutorialFocus::Close,
        fonts,
    );
    paint_button(
        pixels,
        width,
        layout.back_button,
        "Back",
        model.focus == TutorialFocus::Back,
        model.step.previous().is_some(),
        TUTORIAL_BUTTON,
        TUTORIAL_TEXT,
        fonts,
    );
    paint_button(
        pixels,
        width,
        layout.skip_button,
        "Skip",
        model.focus == TutorialFocus::Skip,
        true,
        TUTORIAL_BUTTON,
        TUTORIAL_TEXT,
        fonts,
    );
    paint_button(
        pixels,
        width,
        layout.next_button,
        if model.step == TutorialStep::last() {
            "Done"
        } else {
            "Next"
        },
        model.focus == TutorialFocus::Next,
        true,
        TUTORIAL_ACCENT,
        [255, 255, 255, 255],
        fonts,
    );

    let control_hint = if model.reduced_motion {
        "Tab focus  Enter select  Esc close  Motion reduced"
    } else {
        "Tab focus  Enter select  Esc close"
    };
    fonts.draw_text(
        pixels,
        width,
        layout.control_hint,
        TUTORIAL_MUTED,
        control_hint,
        9.0,
    );
}

fn step_eyebrow(step: TutorialStep) -> &'static str {
    match step {
        TutorialStep::TopBar => "STEP 1 OF 6",
        TutorialStep::Dock => "STEP 2 OF 6",
        TutorialStep::AppGallery => "STEP 3 OF 6",
        TutorialStep::Notifications => "STEP 4 OF 6",
        TutorialStep::Windows => "STEP 5 OF 6",
        TutorialStep::Terminal => "STEP 6 OF 6",
    }
}

fn paint_button(
    pixels: &mut [u8],
    width: usize,
    rect: Rect,
    label: &str,
    focused: bool,
    enabled: bool,
    background: [u8; 4],
    foreground: [u8; 4],
    fonts: &FontBook,
) {
    if focused {
        fill_round_rect(
            pixels,
            width,
            Rect::new(
                rect.origin.x.saturating_sub(3),
                rect.origin.y.saturating_sub(3),
                rect.size.width.saturating_add(6),
                rect.size.height.saturating_add(6),
            ),
            12,
            TUTORIAL_FOCUS,
        );
    }
    let colour = if enabled { background } else { [196, 211, 220, 220] };
    fill_round_rect(pixels, width, rect, 9, colour);
    let text_colour = if enabled { foreground } else { TUTORIAL_MUTED };
    let text_width = fonts.text_width(label, 10.0);
    fonts.draw_text(
        pixels,
        width,
        Rect::new(
            rect.origin.x + (rect.size.width - text_width) / 2,
            rect.origin.y + 10,
            text_width.saturating_add(4),
            14,
        ),
        text_colour,
        label,
        10.0,
    );
}

fn paint_close_button(pixels: &mut [u8], width: usize, rect: Rect, focused: bool, fonts: &FontBook) {
    if focused {
        fill_round_rect(
            pixels,
            width,
            Rect::new(
                rect.origin.x.saturating_sub(3),
                rect.origin.y.saturating_sub(3),
                rect.size.width.saturating_add(6),
                rect.size.height.saturating_add(6),
            ),
            9,
            TUTORIAL_FOCUS,
        );
    }
    fill_round_rect(pixels, width, rect, 7, TUTORIAL_BUTTON);
    let text_width = fonts.text_width("x", 13.0);
    fonts.draw_text(
        pixels,
        width,
        Rect::new(
            rect.origin.x + (rect.size.width - text_width) / 2,
            rect.origin.y + 5,
            text_width.saturating_add(3),
            14,
        ),
        TUTORIAL_TEXT,
        "x",
        13.0,
    );
}

fn paint_illustration(pixels: &mut [u8], width: usize, rect: Rect, target: TutorialTarget) {
    let inset = 16;
    let inner = Rect::new(
        rect.origin.x + inset,
        rect.origin.y + inset,
        rect.size.width.saturating_sub(inset * 2),
        rect.size.height.saturating_sub(inset * 2),
    );
    let panel = [242, 250, 255, 188];
    let dark = [20, 66, 91, 220];
    let bright = [255, 255, 255, 230];
    fill_round_rect(pixels, width, inner, 12, panel);

    match target {
        TutorialTarget::TopBar => {
            fill_round_rect(
                pixels,
                width,
                Rect::new(inner.origin.x, inner.origin.y, inner.size.width, 20),
                6,
                dark,
            );
            for index in 0..3 {
                fill_circle(
                    pixels,
                    width,
                    inner.origin.x + 10 + index * 10,
                    inner.origin.y + 10,
                    3,
                    bright,
                );
            }
            fill_round_rect(
                pixels,
                width,
                Rect::new(inner.right().saturating_sub(46), inner.origin.y + 7, 36, 6),
                3,
                TUTORIAL_ACCENT,
            );
            fill_round_rect(
                pixels,
                width,
                Rect::new(inner.origin.x + 10, inner.origin.y + 42, inner.size.width - 20, 7),
                3,
                [128, 178, 199, 180],
            );
        }
        TutorialTarget::Dock => {
            let dock = Rect::new(
                inner.origin.x + 8,
                inner.bottom().saturating_sub(36),
                inner.size.width.saturating_sub(16),
                24,
            );
            fill_round_rect(pixels, width, dock, 10, dark);
            for index in 0..4 {
                fill_round_rect(
                    pixels,
                    width,
                    Rect::new(dock.origin.x + 8 + index * 25, dock.origin.y + 5, 14, 14),
                    4,
                    if index == 1 { bright } else { TUTORIAL_ACCENT_SOFT },
                );
            }
            fill_circle(
                pixels,
                width,
                dock.origin.x + 15 + 25,
                dock.origin.y.saturating_sub(5),
                4,
                TUTORIAL_ACCENT,
            );
        }
        TutorialTarget::AppGallery => {
            let cell_width = ((inner.size.width.saturating_sub(12)) / 2).max(1);
            for row in 0..2 {
                for column in 0..2 {
                    let cell = Rect::new(
                        inner.origin.x + column * (cell_width + 6),
                        inner.origin.y + row * 36,
                        cell_width,
                        28,
                    );
                    fill_round_rect(
                        pixels,
                        width,
                        cell,
                        6,
                        if row == 0 && column == 0 {
                            TUTORIAL_ACCENT_SOFT
                        } else {
                            bright
                        },
                    );
                    fill_round_rect(
                        pixels,
                        width,
                        Rect::new(cell.origin.x + 6, cell.origin.y + 7, 12, 12),
                        3,
                        if row == 0 && column == 0 {
                            TUTORIAL_ACCENT
                        } else {
                            dark
                        },
                    );
                }
            }
        }
        TutorialTarget::Notifications => {
            for index in 0..2 {
                let card = Rect::new(
                    inner.origin.x + 8,
                    inner.origin.y + 4 + index * 34,
                    inner.size.width.saturating_sub(16),
                    26,
                );
                fill_round_rect(
                    pixels,
                    width,
                    card,
                    7,
                    if index == 0 { TUTORIAL_ACCENT_SOFT } else { bright },
                );
                fill_circle(
                    pixels,
                    width,
                    card.origin.x + 12,
                    card.origin.y + 13,
                    4,
                    if index == 0 { TUTORIAL_ACCENT } else { dark },
                );
                fill_round_rect(
                    pixels,
                    width,
                    Rect::new(card.origin.x + 24, card.origin.y + 9, card.size.width - 34, 6),
                    3,
                    [104, 153, 177, 180],
                );
            }
        }
        TutorialTarget::Windows => {
            let back = Rect::new(
                inner.origin.x + 16,
                inner.origin.y + 12,
                inner.size.width - 26,
                inner.size.height - 22,
            );
            let front = Rect::new(
                inner.origin.x + 4,
                inner.origin.y + 26,
                inner.size.width - 26,
                inner.size.height - 22,
            );
            fill_round_rect(pixels, width, back, 7, [175, 215, 228, 210]);
            fill_round_rect(pixels, width, front, 7, bright);
            fill_round_rect(
                pixels,
                width,
                Rect::new(front.origin.x + 8, front.origin.y + 8, front.size.width - 16, 6),
                3,
                TUTORIAL_ACCENT,
            );
            fill_round_rect(
                pixels,
                width,
                Rect::new(front.origin.x + 8, front.origin.y + 24, front.size.width - 30, 6),
                3,
                [120, 169, 188, 180],
            );
        }
        TutorialTarget::Terminal => {
            fill_round_rect(pixels, width, inner, 9, dark);
            for index in 0..3 {
                fill_circle(
                    pixels,
                    width,
                    inner.origin.x + 12 + index * 11,
                    inner.origin.y + 10,
                    3,
                    if index == 0 { [248, 108, 103, 255] } else { bright },
                );
            }
            fill_round_rect(
                pixels,
                width,
                Rect::new(inner.origin.x + 12, inner.origin.y + 34, inner.size.width - 24, 6),
                3,
                [113, 201, 220, 230],
            );
            fill_round_rect(
                pixels,
                width,
                Rect::new(inner.origin.x + 12, inner.origin.y + 52, inner.size.width - 42, 6),
                3,
                [128, 178, 199, 180],
            );
            fill_round_rect(
                pixels,
                width,
                Rect::new(
                    inner.right().saturating_sub(30),
                    inner.bottom().saturating_sub(16),
                    18,
                    4,
                ),
                2,
                TUTORIAL_ACCENT,
            );
        }
    }
}

/// Paint the first-run film frame.
///
/// The composition: the ocean backdrop fades out into the desktop; the Rouch
/// mark rises into place above centre; the wordmark fades in beneath it.
fn paint_first_run(pixels: &mut [u8], width: usize, work_area: Rect, frame: WelcomeFrame) {
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 0]);
    }

    let opacity = 1.0 - frame.desktop_progress;
    if opacity <= 0.0 {
        return;
    }
    let alpha = (opacity * 255.0) as u8;

    let height = (pixels.len() / (width * 4).max(1)) as i32;
    fill_round_rect(
        pixels,
        width,
        Rect::new(0, 0, width as i32, height),
        0,
        [OCEAN[0], OCEAN[1], OCEAN[2], alpha],
    );

    let layout = first_run_layout(work_area);
    let mark_alpha = (frame.mark_progress.min(1.0) * 255.0) as u8;

    // The mark rises from below its resting place as progress grows.
    let rise = (1.0 - frame.mark_progress.min(1.0)) * 60.0;
    let mark_size = 96;
    let mark = Rect::new(
        layout.mark_centre.x - mark_size / 2,
        layout.mark_centre.y - mark_size / 2 + rise as i32,
        mark_size,
        mark_size,
    );

    // Draw the mark as concentric rounded squares fading in.
    fill_round_rect(
        pixels,
        width,
        mark,
        24,
        [MARK_FACE[0], MARK_FACE[1], MARK_FACE[2], mark_alpha],
    );
    let core = Rect::new(
        mark.origin.x + 18,
        mark.origin.y + 18,
        mark.size.width - 36,
        mark.size.height - 36,
    );
    fill_round_rect(
        pixels,
        width,
        core,
        18,
        [MARK_CORE[0], MARK_CORE[1], MARK_CORE[2], mark_alpha],
    );

    // The wordmark below the mark.
    let word_alpha = (frame.wordmark_progress.min(1.0) * 255.0) as u8;
    if word_alpha > 0 {
        let label = "Rouch";
        let label_w = text_width(label);
        let text_rect = Rect::new(
            layout.wordmark_rect.origin.x + (layout.wordmark_rect.size.width - label_w) / 2,
            layout.wordmark_rect.origin.y + (layout.wordmark_rect.size.height - 7) / 2,
            label_w + 4,
            7,
        );
        draw_text(
            pixels,
            width,
            text_rect,
            [WORDMARK[0], WORDMARK[1], WORDMARK[2], word_alpha],
            label,
        );
    }

    // A subtle progress hint at the bottom while the film plays.
    if !frame.finished {
        let dot_y = work_area.bottom() - 60;
        for i in -1..=1 {
            let phase = (frame.mark_progress + frame.wordmark_progress) / 2.0;
            let dot_alpha = (phase * 200.0) as u8 / 3;
            let cx = work_area.origin.x + work_area.size.width / 2 + i * 16;
            fill_circle(
                pixels,
                width,
                cx,
                dot_y,
                3,
                [MARK_CORE[0], MARK_CORE[1], MARK_CORE[2], dot_alpha],
            );
        }
    }
}

/// Paint the update card frame.
fn paint_update(pixels: &mut [u8], width: usize, work_area: Rect, frame: UpdateFrame, notes: &ReleaseNotes) {
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 0]);
    }

    let layout = update_layout(work_area);

    // The card slides in from the right by its remaining distance.
    let slide = (1.0 - frame.card_progress) * 220.0;
    let card = Rect::new(
        (layout.card.origin.x as f32 + slide) as i32,
        layout.card.origin.y,
        layout.card.size.width,
        layout.card.size.height,
    );
    fill_round_rect(pixels, width, card, 18, CARD);

    // Soft shadow band under the card, drawn as a translucent edge.
    let shadow = Rect::new(card.origin.x + 10, card.bottom(), card.size.width - 20, 8);
    fill_round_rect(pixels, width, shadow, 4, [10, 14, 22, 60]);

    // Edition mark on the left: a squircle with a wave core, like the
    // release marks Apple paints for each macOS edition.
    fill_round_rect(pixels, width, layout.mark, 28, MARK_FACE);
    let core = Rect::new(
        layout.mark.origin.x + 22,
        layout.mark.origin.y + 22,
        layout.mark.size.width - 44,
        layout.mark.size.height - 44,
    );
    fill_round_rect(pixels, width, core, 20, MARK_CORE);

    // Title: "Rouch Ocean 0.2.0", matching macOS's edition titling.
    let title = format!("{} {}", notes.edition, notes.version);
    let title_w = text_width(&title);
    draw_text(
        pixels,
        width,
        Rect::new(
            layout.title.origin.x + (layout.title.size.width - title_w) / 2,
            layout.title.origin.y,
            title_w + 4,
            7,
        ),
        CARD_TITLE,
        &title,
    );

    // Changelog entries, one per line, with an accent dash.
    let mut y = layout.entries.origin.y;
    for highlight in notes.highlights {
        if y + 10 > layout.entries.bottom() {
            break;
        }
        fill_round_rect(
            pixels,
            width,
            Rect::new(layout.entries.origin.x, y + 2, 6, 2),
            1,
            CARD_ACCENT,
        );
        draw_text(
            pixels,
            width,
            Rect::new(layout.entries.origin.x + 14, y, layout.entries.size.width - 14, 7),
            CARD_TEXT,
            highlight,
        );
        y += 22;
    }

    // Continue button.
    fill_round_rect(pixels, width, layout.button, 12, CARD_BUTTON);
    let label = "Continue";
    let label_w = text_width(label);
    draw_text(
        pixels,
        width,
        Rect::new(
            layout.button.origin.x + (layout.button.size.width - label_w) / 2,
            layout.button.origin.y + (layout.button.size.height - 7) / 2,
            label_w + 4,
            7,
        ),
        CARD_BUTTON_TEXT,
        label,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::welcome::{TutorialAction, TutorialStatus};

    #[test]
    fn visual_mode_uses_opaque_material_for_policy_or_low_end() {
        assert_eq!(
            TutorialVisualMode::for_constraints(false, false),
            TutorialVisualMode::LiquidGlass
        );
        assert_eq!(
            TutorialVisualMode::for_constraints(true, false),
            TutorialVisualMode::Opaque
        );
        assert_eq!(
            TutorialVisualMode::for_constraints(false, true),
            TutorialVisualMode::Opaque
        );
    }

    #[test]
    fn render_model_stays_hidden_after_skip_and_resumes_cleanly() {
        let mut state = TutorialState::new();
        state.apply(TutorialAction::Skip);
        assert_eq!(state.status, TutorialStatus::Skipped);
        assert!(tutorial_render_model(state, TutorialVisualMode::LiquidGlass).is_none());

        state.apply(TutorialAction::Resume);
        let model = tutorial_render_model(state, TutorialVisualMode::Opaque).unwrap();
        assert_eq!(model.step, TutorialStep::TopBar);
        assert_eq!(model.visual_mode, TutorialVisualMode::Opaque);
    }

    #[test]
    fn layout_and_renderer_model_keep_focus_as_render_data() {
        let mut state = TutorialState::new();
        state.apply(TutorialAction::FocusNext);
        let model = tutorial_render_model(state, TutorialVisualMode::LiquidGlass).unwrap();
        assert_eq!(model.focus, TutorialFocus::Close);

        let layout = tutorial_layout(Rect::new(32, 18, 1280, 720));
        assert_eq!(layout.backdrop.origin, crate::windowing::Point::new(32, 18));
        assert!(layout.card.size.width > 0);
        assert!(layout.card.size.height > 0);
    }
}
