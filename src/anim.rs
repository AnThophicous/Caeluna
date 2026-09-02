//! Easing and animation timing for Liquid Material window transitions.
//!
//! macOS Tahoe windows scale up when they open, slide and scale into the dock
//! when they minimize, and reverse that on restore. The values here are pure:
//! the renderer samples `progress(now)` and never owns animation state itself.

use std::time::Duration;

/// A window transition driven by wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// A newly opened window grows from a midair seed.
    Open,
    /// The window shrinks toward its dock target.
    Minimize,
    /// The window un-shrinks back to its frame.
    Restore,
    /// Fades away while scaling down slightly, for closing windows.
    Close,
}

impl Transition {
    /// Total duration of the transition.
    pub const fn duration(self) -> Duration {
        match self {
            Self::Open => Duration::from_millis(320),
            Self::Minimize => Duration::from_millis(280),
            Self::Restore => Duration::from_millis(280),
            Self::Close => Duration::from_millis(200),
        }
    }

    /// Where the window is heading while it animates. For minimize this is the
    /// centre of the dock strip, which is where the dock will live.
    pub const fn target(self) -> Target {
        match self {
            Self::Open | Self::Restore => Target::WindowFrame,
            Self::Minimize => Target::DockStrip,
            Self::Close => Target::WindowFrame,
        }
    }
}

/// What a transition interpolates its geometry toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The window's own restore frame.
    WindowFrame,
    /// The horizontal strip at the bottom of the work area.
    DockStrip,
}

/// Normalized animation progress sampled from elapsed time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// 0.0 at the start, 1.0 when finished.
    pub value: f32,
    finished: bool,
}

impl Progress {
    /// Advance `elapsed` along `transition`.
    ///
    /// Values overshoot above 1.0 during an opening bounce, so a caller must
    /// still stop the animation once `finished` even if `value > 1.0`.
    pub fn elapsed(transition: Transition, elapsed: Duration) -> Self {
        let total = transition.duration();
        let raw = if total.is_zero() {
            1.0
        } else {
            elapsed.as_secs_f32() / total.as_secs_f32()
        };

        let finished = raw >= 1.0;
        let value = match transition {
            Transition::Open => back_out(raw.min(1.0)),
            Transition::Minimize | Transition::Restore => ease_in_out(raw.min(1.0)),
            Transition::Close => ease_in(raw.min(1.0)),
        };

        Self { value, finished }
    }

    pub fn finished(self) -> bool {
        self.finished
    }
}

/// Smooth accelerate/decelerate curve, the default macOS feel.
fn ease_in_out(t: f32) -> f32 {
    if t <= 0.5 {
        2.0 * t * t
    } else {
        let u = t - 1.0;
        1.0 - 2.0 * u * u
    }
}

/// Gentle acceleration for fades that end early.
fn ease_in(t: f32) -> f32 {
    t * t
}

/// A soft overshoot used for opening windows: they pass their final size by
/// a few percent and settle back, like a macOS launch bounce.
fn back_out(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    let u = t - 1.0;
    1.0 + C3 * u * u * u + C1 * u * u
}

/// Interpolate a scale factor an animated window should be drawn with.
///
/// Minimize starts at 1.0 and ends at the dock's small scale; open and restore
/// start small and end at 1.0.
pub fn interpolated_scale(transition: Transition, progress: Progress) -> f32 {
    const DOCK_END_SCALE: f32 = 0.15;
    let t = progress.value;

    match transition {
        Transition::Open | Transition::Restore => DOCK_END_SCALE + (1.0 - DOCK_END_SCALE) * t,
        Transition::Minimize => 1.0 + (DOCK_END_SCALE - 1.0) * t,
        Transition::Close => 1.0 - 0.1 * t,
    }
}

/// Interpolated opacity for transitions that fade.
pub fn interpolated_opacity(transition: Transition, progress: Progress) -> f32 {
    let t = progress.value;
    match transition {
        Transition::Close => 1.0 - t.clamp(0.0, 1.0),
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_starts_at_zero_and_finishes_at_one() {
        let open = Progress::elapsed(Transition::Open, Duration::ZERO);
        assert!((open.value).abs() < 0.2);
        assert!(!open.finished());

        let done = Progress::elapsed(Transition::Open, Transition::Open.duration() * 2);
        assert!(done.finished());
        assert!((done.value - 1.0).abs() < 0.1);
    }

    #[test]
    fn back_out_overshoots_in_the_middle_and_settles() {
        assert!(back_out(0.5) > 1.0);
        assert!((back_out(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn minimize_shrinks_and_restore_grows() {
        let start = Progress::elapsed(Transition::Minimize, Duration::ZERO);
        let end = Progress::elapsed(Transition::Minimize, Transition::Minimize.duration() * 3);

        assert!((interpolated_scale(Transition::Minimize, start) - 1.0).abs() < 0.05);
        assert!(interpolated_scale(Transition::Minimize, end) < 0.3);

        let restore_end = Progress::elapsed(Transition::Restore, Transition::Restore.duration() * 3);
        assert!((interpolated_scale(Transition::Restore, restore_end) - 1.0).abs() < 0.01);
    }

    #[test]
    fn close_fades_to_zero() {
        let end = Progress::elapsed(Transition::Close, Transition::Close.duration() * 3);
        assert!((interpolated_opacity(Transition::Close, end) as f64) < 1e-6);
    }

    #[test]
    fn minimize_targets_the_dock_strip() {
        assert_eq!(Transition::Minimize.target(), Target::DockStrip);
        assert_eq!(Transition::Open.target(), Target::WindowFrame);
    }
}
