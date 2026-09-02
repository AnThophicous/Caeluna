//! Renderer for the first-run Rouch environment setup card.
//!
//! This is deliberately a small, opaque overlay: the setup flow is a
//! recoverable operation surface, not an animated marketing welcome screen.

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
    pixel::{fill_circle, fill_rect, fill_round_rect},
};
use crate::{
    setup::{SetupState, SetupStatus, SetupStep},
    windowing::Rect,
};

const BACKDROP: [u8; 4] = [5, 19, 38, 190];
const CARD: [u8; 4] = [242, 248, 252, 255];
const TEXT: [u8; 4] = [19, 36, 52, 255];
const MUTED: [u8; 4] = [88, 109, 124, 255];
const ACCENT: [u8; 4] = [8, 127, 178, 255];
const ACCENT_SOFT: [u8; 4] = [214, 239, 248, 255];
const ERROR: [u8; 4] = [185, 54, 63, 255];
const DONE: [u8; 4] = [43, 155, 96, 255];

/// A reusable output-sized setup overlay.
pub struct SetupRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
}

/// Geometry of the setup card shared with pointer hit-testing.
pub fn card_rect(work_area: Rect) -> Rect {
    let width = work_area.size.width.max(1);
    let height = work_area.size.height.max(1);
    let card_w = 720.min(width.saturating_sub(48)).max(0);
    let card_h = 520.min(height.saturating_sub(48)).max(0);
    Rect::new(
        work_area.origin.x + (width - card_w) / 2,
        work_area.origin.y + (height - card_h) / 2,
        card_w,
        card_h,
    )
}

/// Primary action hit box for the setup state machine.
pub fn button_rect(work_area: Rect) -> Rect {
    let card = card_rect(work_area);
    Rect::new(card.right() - 184, card.bottom() - 60, 142, 36)
}

impl Default for SetupRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupRenderer {
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

    pub fn element<R>(
        &mut self,
        renderer: &mut R,
        work_area: Rect,
        state: &SetupState,
        fonts: &FontBook,
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
            let _ = context.draw(|pixels| {
                let w = width as usize;
                fill_rect(pixels, w, Rect::new(0, 0, width, height), BACKDROP);
                let global_card = card_rect(work_area);
                let card = Rect::new(
                    global_card.origin.x - work_area.origin.x,
                    global_card.origin.y - work_area.origin.y,
                    global_card.size.width,
                    global_card.size.height,
                );
                fill_round_rect(pixels, w, card, 22, CARD);

                fonts.draw_text(
                    pixels,
                    w,
                    Rect::new(card.origin.x + 42, card.origin.y + 34, card.size.width - 84, 26),
                    TEXT,
                    "Set up your Rouch environment",
                    24.0,
                );
                fonts.draw_text(
                    pixels,
                    w,
                    Rect::new(card.origin.x + 42, card.origin.y + 70, card.size.width - 84, 14),
                    MUTED,
                    "Choose how Rouch should start, render, and recover.",
                    12.0,
                );

                let steps = SetupStep::actionable();
                let list_left = card.origin.x + 42;
                let list_top = card.origin.y + 120;
                for (index, step) in steps.iter().enumerate() {
                    let y = list_top + index as i32 * 42;
                    let completed = state.completed.contains(step) || state.skipped.contains(step);
                    let current = state.current_step == *step && !state.is_complete();
                    let colour = if completed {
                        DONE
                    } else if current {
                        ACCENT
                    } else {
                        MUTED
                    };
                    fill_circle(
                        pixels,
                        w,
                        list_left + 9,
                        y + 10,
                        8,
                        if current { ACCENT_SOFT } else { colour },
                    );
                    if completed {
                        fill_circle(pixels, w, list_left + 9, y + 10, 4, DONE);
                    }
                    let label = step_label(*step);
                    fonts.draw_text(
                        pixels,
                        w,
                        Rect::new(list_left + 28, y + 3, 250, 14),
                        colour,
                        label,
                        12.0,
                    );
                    if current {
                        let hint = step_hint(*step);
                        fonts.draw_text(
                            pixels,
                            w,
                            Rect::new(list_left + 28, y + 20, 390, 11),
                            MUTED,
                            hint,
                            9.0,
                        );
                    }
                }

                let right = card.origin.x + 430;
                let right_top = card.origin.y + 132;
                let status = match state.status {
                    SetupStatus::NotStarted => "Ready to begin",
                    SetupStatus::InProgress => "Current step needs your decision",
                    SetupStatus::Failed => "This step needs attention",
                    SetupStatus::Complete => "Rouch is ready",
                };
                fonts.draw_text(
                    pixels,
                    w,
                    Rect::new(right, right_top, 240, 16),
                    if state.status == SetupStatus::Failed {
                        ERROR
                    } else {
                        TEXT
                    },
                    status,
                    14.0,
                );
                if let Some(error) = state.last_error.as_deref() {
                    fill_round_rect(
                        pixels,
                        w,
                        Rect::new(right, right_top + 38, card.right() - right - 42, 76),
                        10,
                        [255, 235, 236, 255],
                    );
                    fonts.draw_text(
                        pixels,
                        w,
                        Rect::new(right + 14, right_top + 52, card.right() - right - 70, 50),
                        ERROR,
                        &truncate(error, 56),
                        10.0,
                    );
                } else {
                    let progress = state.completed.len() + state.skipped.len();
                    let progress_text = format!("{progress}/{} stages accounted for", steps.len());
                    fonts.draw_text(
                        pixels,
                        w,
                        Rect::new(right, right_top + 38, 260, 14),
                        MUTED,
                        &progress_text,
                        11.0,
                    );
                    let track = Rect::new(right, right_top + 72, card.right() - right - 42, 8);
                    fill_round_rect(pixels, w, track, 4, [211, 223, 231, 255]);
                    let fill = Rect::new(
                        track.origin.x,
                        track.origin.y,
                        (track.size.width * progress as i32 / steps.len().max(1) as i32)
                            .clamp(0, track.size.width),
                        track.size.height,
                    );
                    fill_round_rect(pixels, w, fill, 4, ACCENT);
                }

                let footer = card.bottom() - 60;
                if state.status == SetupStatus::Complete {
                    fill_round_rect(
                        pixels,
                        w,
                        Rect::new(card.right() - 184, footer, 142, 36),
                        10,
                        ACCENT_SOFT,
                    );
                    fonts.draw_text(
                        pixels,
                        w,
                        Rect::new(card.right() - 154, footer + 12, 100, 12),
                        ACCENT,
                        "Continue",
                        11.0,
                    );
                } else if state.status == SetupStatus::Failed {
                    fill_round_rect(
                        pixels,
                        w,
                        Rect::new(card.right() - 184, footer, 142, 36),
                        10,
                        ACCENT_SOFT,
                    );
                    fonts.draw_text(
                        pixels,
                        w,
                        Rect::new(card.right() - 158, footer + 12, 110, 12),
                        ACCENT,
                        "Retry step",
                        11.0,
                    );
                } else {
                    let action = if state.status == SetupStatus::NotStarted {
                        "Begin setup"
                    } else {
                        "Apply step"
                    };
                    fill_round_rect(
                        pixels,
                        w,
                        Rect::new(card.right() - 184, footer, 142, 36),
                        10,
                        ACCENT_SOFT,
                    );
                    fonts.draw_text(
                        pixels,
                        w,
                        Rect::new(card.right() - 164, footer + 12, 124, 12),
                        ACCENT,
                        action,
                        11.0,
                    );
                }
                if state.current_step.is_optional() && state.status == SetupStatus::InProgress {
                    fonts.draw_text(
                        pixels,
                        w,
                        Rect::new(card.origin.x + 42, footer + 12, 190, 12),
                        MUTED,
                        "Press Escape to skip this optional step",
                        9.0,
                    );
                }

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

fn step_label(step: SetupStep) -> &'static str {
    match step {
        SetupStep::Welcome => "Confirm setup",
        SetupStep::Dependencies => "Check dependencies",
        SetupStep::GraphicsBackend => "Choose renderer",
        SetupStep::Swapfile => "Review swapfile",
        SetupStep::DesktopEntry => "Install app entry",
        SetupStep::Autostart => "Start with system",
        SetupStep::InterfaceStage1 => "Activate first interface stage",
        SetupStep::Complete => "Complete",
    }
}

fn step_hint(step: SetupStep) -> &'static str {
    match step {
        SetupStep::Welcome => "Rouch will keep this setup resumable.",
        SetupStep::Dependencies => "Only the runtime pieces needed by the first stage are installed.",
        SetupStep::GraphicsBackend => "Vulkan is preferred; OpenGL keeps the desktop usable.",
        SetupStep::Swapfile => "Recommended for memory pressure; changing an active file needs confirmation.",
        SetupStep::DesktopEntry => "Adds Rouch to the desktop application menu.",
        SetupStep::Autostart => "Optional: launch Rouch when your graphical session starts.",
        SetupStep::InterfaceStage1 => "Downloads the shell bootstrap, not the whole catalog or media set.",
        SetupStep::Complete => "",
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.pop();
        output.push('…');
    }
    output
}
