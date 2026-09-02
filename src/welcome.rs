//! First-run and update welcome experience, macOS-style.
//!
//! Two pure models describe the whole flow:
//!
//! - On a fresh install (no state file, or no remembered version), Rouch
//!   plays an animated welcome screen: the Rouch mark rises, the wordmark
//!   fades in, then the desktop fades in beneath it.
//! - On an update (state file remembers an older version), a welcome *card*
//!   slides in over the desktop: the edition mark on the left, the release
//!   title and its changelog entries on the right.
//!
//! All geometry and copy is decided here so the renderer and the input path
//! agree pixel-for-pixel.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Which welcome experience this session starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WelcomeStage {
    /// Fresh install: the full-screen animated welcome.
    FirstRun,
    /// Updated install: the release-notes card over the desktop.
    Update,
    /// Returning session: straight to the desktop.
    Desktop,
}

impl WelcomeStage {
    /// Decide the stage from the installed and running versions.
    ///
    /// A missing remembered version is a first run. An equal version is a
    /// returning session. Anything else is an update.
    pub fn decide(installed: Option<&str>, running: &str) -> Self {
        match installed {
            None => Self::FirstRun,
            Some(version) if version == running => Self::Desktop,
            Some(_) => Self::Update,
        }
    }
}

/// The short, resumable tour of the desktop shell.
///
/// The order follows the way a new user discovers the desktop: orientation,
/// launching, communication, window movement, and finally the built-in
/// terminal. Keeping this as a small enum makes the state bounded and lets an
/// input backend map Tab, arrows, Enter and Escape without knowing any copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialStep {
    TopBar,
    Dock,
    AppGallery,
    Notifications,
    Windows,
    Terminal,
}

/// All tutorial steps in presentation order.
pub const TUTORIAL_STEPS: &[TutorialStep; 6] = &[
    TutorialStep::TopBar,
    TutorialStep::Dock,
    TutorialStep::AppGallery,
    TutorialStep::Notifications,
    TutorialStep::Windows,
    TutorialStep::Terminal,
];

impl TutorialStep {
    pub const fn first() -> Self {
        Self::TopBar
    }

    pub const fn last() -> Self {
        Self::Terminal
    }

    pub const fn index(self) -> usize {
        match self {
            Self::TopBar => 0,
            Self::Dock => 1,
            Self::AppGallery => 2,
            Self::Notifications => 3,
            Self::Windows => 4,
            Self::Terminal => 5,
        }
    }

    pub const fn number(self) -> usize {
        self.index() + 1
    }

    pub const fn next(self) -> Option<Self> {
        match self {
            Self::TopBar => Some(Self::Dock),
            Self::Dock => Some(Self::AppGallery),
            Self::AppGallery => Some(Self::Notifications),
            Self::Notifications => Some(Self::Windows),
            Self::Windows => Some(Self::Terminal),
            Self::Terminal => None,
        }
    }

    pub const fn previous(self) -> Option<Self> {
        match self {
            Self::TopBar => None,
            Self::Dock => Some(Self::TopBar),
            Self::AppGallery => Some(Self::Dock),
            Self::Notifications => Some(Self::AppGallery),
            Self::Windows => Some(Self::Notifications),
            Self::Terminal => Some(Self::Windows),
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::TopBar => "top-bar",
            Self::Dock => "dock",
            Self::AppGallery => "app-gallery",
            Self::Notifications => "notifications",
            Self::Windows => "windows",
            Self::Terminal => "terminal",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "top-bar" => Some(Self::TopBar),
            "dock" => Some(Self::Dock),
            "app-gallery" => Some(Self::AppGallery),
            "notifications" => Some(Self::Notifications),
            "windows" => Some(Self::Windows),
            "terminal" => Some(Self::Terminal),
            _ => None,
        }
    }

    /// Copy-friendly content for both raster rendering and accessibility
    /// announcements. Lines are deliberately short so the no-font fallback
    /// can draw them without wrapping allocations.
    pub const fn content(self) -> TutorialContent {
        match self {
            Self::TopBar => TutorialContent {
                title: "Read the top bar",
                body: &[
                    "The left side shows the current app and menu.",
                    "The right side keeps time, network and battery close.",
                ],
                shortcut: "Control Centre: Super C",
                target: TutorialTarget::TopBar,
            },
            Self::Dock => TutorialContent {
                title: "Use the Dock",
                body: &[
                    "Open favorite apps from the Dock at the bottom.",
                    "Point at an icon to see its name and windows.",
                ],
                shortcut: "Launcher: Super Space",
                target: TutorialTarget::Dock,
            },
            Self::AppGallery => TutorialContent {
                title: "Get apps from App Gallery",
                body: &[
                    "App Gallery finds apps from Flatpak and Flathub.",
                    "Choose an app, review its details, then install it.",
                ],
                shortcut: "Open App Gallery from the Dock",
                target: TutorialTarget::AppGallery,
            },
            Self::Notifications => TutorialContent {
                title: "Keep notifications in your hands",
                body: &[
                    "Open Notification Centre from the top bar.",
                    "Use Settings / Notifications to turn alerts on or off.",
                ],
                shortcut: "Notification Centre: Super N",
                target: TutorialTarget::Notifications,
            },
            Self::Windows => TutorialContent {
                title: "Move between windows",
                body: &[
                    "Hold Alt and press Tab to switch through open windows.",
                    "Use workspaces to keep projects apart and focused.",
                ],
                shortcut: "Workspaces: Super Left / Right",
                target: TutorialTarget::Windows,
            },
            Self::Terminal => TutorialContent {
                title: "Make the terminal yours",
                body: &[
                    "Rouch Terminal runs real commands with UTF-8 ready.",
                    "Use tabs for separate shells and set each tab's title.",
                    "Press Esc to leave; resume in Settings / Tutorial.",
                ],
                shortcut: "Open Terminal: Super Enter",
                target: TutorialTarget::Terminal,
            },
        }
    }
}

/// Static copy for one tutorial page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TutorialContent {
    pub title: &'static str,
    pub body: &'static [&'static str],
    pub shortcut: &'static str,
    pub target: TutorialTarget,
}

/// The shell landmark highlighted by a tutorial page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialTarget {
    TopBar,
    Dock,
    AppGallery,
    Notifications,
    Windows,
    Terminal,
}

/// The control that receives keyboard focus inside the tutorial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialFocus {
    Back,
    Skip,
    Next,
    Close,
}

impl TutorialFocus {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Back => "back",
            Self::Skip => "skip",
            Self::Next => "next",
            Self::Close => "close",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Back => Self::Skip,
            Self::Skip => Self::Next,
            Self::Next => Self::Close,
            Self::Close => Self::Back,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::Back => Self::Close,
            Self::Skip => Self::Back,
            Self::Next => Self::Skip,
            Self::Close => Self::Next,
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "back" => Some(Self::Back),
            "skip" => Some(Self::Skip),
            "next" => Some(Self::Next),
            "close" => Some(Self::Close),
            _ => None,
        }
    }
}

/// Why the tutorial is no longer visible. Dismissed and skipped states keep
/// the current page so the next explicit resume starts where the user left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialStatus {
    Open,
    Dismissed,
    Skipped,
    Completed,
}

impl TutorialStatus {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Dismissed => "dismissed",
            Self::Skipped => "skipped",
            Self::Completed => "completed",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "open" => Some(Self::Open),
            "dismissed" => Some(Self::Dismissed),
            "skipped" => Some(Self::Skipped),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// A small action vocabulary for pointer and keyboard adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialAction {
    Previous,
    Next,
    Skip,
    Close,
    Resume,
    Restart,
    FocusNext,
    FocusPrevious,
    Activate,
    SetReducedMotion(bool),
}

/// Keys that an input adapter can translate without importing a backend's key
/// enum into this pure module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialKey {
    Tab,
    BackTab,
    Left,
    Right,
    Enter,
    Space,
    Escape,
}

impl TutorialKey {
    pub const fn action(self) -> TutorialAction {
        match self {
            Self::Tab => TutorialAction::FocusNext,
            Self::BackTab => TutorialAction::FocusPrevious,
            Self::Left => TutorialAction::Previous,
            Self::Right => TutorialAction::Next,
            Self::Enter | Self::Space => TutorialAction::Activate,
            Self::Escape => TutorialAction::Close,
        }
    }
}

/// Persisted and renderable tutorial state. It is intentionally Copy and
/// contains no unbounded text or collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TutorialState {
    pub step: TutorialStep,
    pub focus: TutorialFocus,
    pub status: TutorialStatus,
    pub reduced_motion: bool,
}

impl Default for TutorialState {
    fn default() -> Self {
        Self::new()
    }
}

impl TutorialState {
    pub const fn new() -> Self {
        Self {
            step: TutorialStep::first(),
            focus: TutorialFocus::Next,
            status: TutorialStatus::Open,
            reduced_motion: false,
        }
    }

    pub const fn is_open(self) -> bool {
        matches!(self.status, TutorialStatus::Open)
    }

    pub const fn can_resume(self) -> bool {
        !self.is_open()
    }

    pub const fn is_finished(self) -> bool {
        !self.is_open()
    }

    pub const fn content(self) -> TutorialContent {
        self.step.content()
    }

    pub const fn progress(self) -> (usize, usize) {
        (self.step.number(), TUTORIAL_STEPS.len())
    }

    /// Apply one action and return a compact change report for redraw and
    /// accessibility-announcement scheduling.
    pub fn apply(&mut self, action: TutorialAction) -> TutorialTransition {
        let previous = *self;
        match action {
            TutorialAction::Previous => self.previous(),
            TutorialAction::Next => self.next(),
            TutorialAction::Skip => self.skip(),
            TutorialAction::Close => self.dismiss(),
            TutorialAction::Resume => self.resume(),
            TutorialAction::Restart => self.restart(),
            TutorialAction::FocusNext => {
                if self.is_open() {
                    self.focus = self.focus.next();
                }
            }
            TutorialAction::FocusPrevious => {
                if self.is_open() {
                    self.focus = self.focus.previous();
                }
            }
            TutorialAction::Activate => {
                if self.is_open() {
                    match self.focus {
                        TutorialFocus::Back => self.previous(),
                        TutorialFocus::Skip => self.skip(),
                        TutorialFocus::Next => self.next(),
                        TutorialFocus::Close => self.dismiss(),
                    }
                }
            }
            TutorialAction::SetReducedMotion(enabled) => self.reduced_motion = enabled,
        }

        TutorialTransition {
            previous_step: previous.step,
            step: self.step,
            previous_status: previous.status,
            status: self.status,
            previous_focus: previous.focus,
            focus: self.focus,
            changed: previous != *self,
        }
    }

    pub fn next(&mut self) {
        if !self.is_open() {
            return;
        }
        if let Some(next) = self.step.next() {
            self.step = next;
            self.focus = TutorialFocus::Next;
        } else {
            self.status = TutorialStatus::Completed;
        }
    }

    pub fn previous(&mut self) {
        if !self.is_open() {
            return;
        }
        if let Some(previous) = self.step.previous() {
            self.step = previous;
            self.focus = TutorialFocus::Next;
        }
    }

    pub fn skip(&mut self) {
        if self.is_open() {
            self.status = TutorialStatus::Skipped;
        }
    }

    pub fn dismiss(&mut self) {
        if self.is_open() {
            self.status = TutorialStatus::Dismissed;
        }
    }

    /// Reopen at the saved page. A completed tour intentionally starts over,
    /// which makes "show tutorial again" predictable from Settings.
    pub fn resume(&mut self) {
        if self.status == TutorialStatus::Completed {
            self.step = TutorialStep::first();
        }
        self.status = TutorialStatus::Open;
        self.focus = TutorialFocus::Next;
    }

    pub fn restart(&mut self) {
        self.step = TutorialStep::first();
        self.focus = TutorialFocus::Next;
        self.status = TutorialStatus::Open;
    }

    pub fn set_reduced_motion(&mut self, enabled: bool) {
        self.reduced_motion = enabled;
    }

    /// Serialize a small versioned state for an integration-owned atomic
    /// writer. No filesystem access happens here.
    pub fn encode(self) -> String {
        format!(
            "{TUTORIAL_PERSISTENCE_HEADER}\n\
             schema=1\n\
             step={}\n\
             status={}\n\
             focus={}\n\
             reduced-motion={}\n",
            self.step.key(),
            self.status.key(),
            self.focus.key(),
            if self.reduced_motion { 1 } else { 0 },
        )
    }

    /// Parse a state produced by [`Self::encode`]. Unknown future keys are
    /// ignored, while duplicate or malformed known keys fail safely.
    pub fn decode(contents: &str) -> Result<Self, TutorialDecodeError> {
        if contents.len() > MAX_TUTORIAL_STATE_BYTES {
            return Err(TutorialDecodeError::TooLarge);
        }
        let mut lines = contents.lines();
        if lines.next() != Some(TUTORIAL_PERSISTENCE_HEADER) {
            return Err(TutorialDecodeError::InvalidHeader);
        }

        let mut schema = None;
        let mut step = None;
        let mut status = None;
        let mut focus = None;
        let mut reduced_motion = None;
        for (line_index, line) in lines.enumerate() {
            let line_number = line_index + 2;
            if line.trim().is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(TutorialDecodeError::MalformedLine { line: line_number });
            };
            match key {
                "schema" => set_tutorial_field(&mut schema, value, "schema", line_number)?,
                "step" => set_tutorial_field(&mut step, value, "step", line_number)?,
                "status" => set_tutorial_field(&mut status, value, "status", line_number)?,
                "focus" => set_tutorial_field(&mut focus, value, "focus", line_number)?,
                "reduced-motion" => {
                    set_tutorial_field(&mut reduced_motion, value, "reduced-motion", line_number)?
                }
                _ => {}
            }
        }

        let schema = tutorial_field(schema, "schema")?;
        if schema != "1" {
            return Err(TutorialDecodeError::InvalidField {
                field: "schema",
                value: schema.to_owned(),
            });
        }
        let step_key = tutorial_field(step, "step")?;
        let step = TutorialStep::from_key(step_key).ok_or_else(|| TutorialDecodeError::InvalidField {
            field: "step",
            value: step_key.to_owned(),
        })?;
        let status_key = tutorial_field(status, "status")?;
        let status =
            TutorialStatus::from_key(status_key).ok_or_else(|| TutorialDecodeError::InvalidField {
                field: "status",
                value: status_key.to_owned(),
            })?;
        let focus_key = tutorial_field(focus, "focus")?;
        let focus = TutorialFocus::from_key(focus_key).ok_or_else(|| TutorialDecodeError::InvalidField {
            field: "focus",
            value: focus_key.to_owned(),
        })?;
        let reduced_motion = match tutorial_field(reduced_motion, "reduced-motion")? {
            "0" => false,
            "1" => true,
            value => {
                return Err(TutorialDecodeError::InvalidField {
                    field: "reduced-motion",
                    value: value.to_owned(),
                });
            }
        };

        Ok(Self {
            step,
            focus,
            status,
            reduced_motion,
        })
    }

    pub fn persistence_plan(
        self,
        path: impl Into<PathBuf>,
    ) -> Result<TutorialPersistencePlan, TutorialPersistenceError> {
        let path = path.into();
        if path.as_os_str().is_empty() || path.file_name().is_none() {
            return Err(TutorialPersistenceError::InvalidPath { path });
        }
        Ok(TutorialPersistencePlan {
            temporary_path: PathBuf::from(format!("{}.tmp", path.to_string_lossy())),
            path,
            contents: self.encode(),
        })
    }
}

/// The state change returned after an action is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TutorialTransition {
    pub previous_step: TutorialStep,
    pub step: TutorialStep,
    pub previous_status: TutorialStatus,
    pub status: TutorialStatus,
    pub previous_focus: TutorialFocus,
    pub focus: TutorialFocus,
    pub changed: bool,
}

pub const TUTORIAL_PERSISTENCE_HEADER: &str = "rouch-tutorial-v1";
const MAX_TUTORIAL_STATE_BYTES: usize = 256;

/// Data for an integration-owned atomic state write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TutorialPersistencePlan {
    pub path: PathBuf,
    pub temporary_path: PathBuf,
    pub contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TutorialPersistenceError {
    InvalidPath { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TutorialDecodeError {
    TooLarge,
    InvalidHeader,
    MalformedLine { line: usize },
    DuplicateField { field: &'static str, line: usize },
    MissingField { field: &'static str },
    InvalidField { field: &'static str, value: String },
}

pub fn default_tutorial_state_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("state")
        .join("rouch")
        .join("tutorial.state")
}

fn set_tutorial_field<'a>(
    target: &mut Option<&'a str>,
    value: &'a str,
    field: &'static str,
    line: usize,
) -> Result<(), TutorialDecodeError> {
    if target.is_some() {
        return Err(TutorialDecodeError::DuplicateField { field, line });
    }
    *target = Some(value);
    Ok(())
}

fn tutorial_field<'a>(field: Option<&'a str>, name: &'static str) -> Result<&'a str, TutorialDecodeError> {
    field.ok_or(TutorialDecodeError::MissingField { field: name })
}

/// The layout is shared by a future pointer adapter and the renderer. All
/// rectangles use absolute work-area coordinates; the renderer translates
/// them to its local output buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TutorialLayout {
    pub backdrop: crate::windowing::Rect,
    pub card: crate::windowing::Rect,
    pub close_button: crate::windowing::Rect,
    pub progress: crate::windowing::Rect,
    pub illustration: crate::windowing::Rect,
    pub eyebrow: crate::windowing::Rect,
    pub title: crate::windowing::Rect,
    pub body: crate::windowing::Rect,
    pub shortcut: crate::windowing::Rect,
    pub control_hint: crate::windowing::Rect,
    pub back_button: crate::windowing::Rect,
    pub skip_button: crate::windowing::Rect,
    pub next_button: crate::windowing::Rect,
}

/// Pointer targets exposed by [`tutorial_control_hit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialControl {
    Close,
    Back,
    Skip,
    Next,
}

pub fn tutorial_layout(work_area: crate::windowing::Rect) -> TutorialLayout {
    let width = work_area.size.width.max(1);
    let height = work_area.size.height.max(1);
    let card_width = if width >= 900 {
        760.min(width.saturating_sub(48))
    } else {
        width.saturating_sub(24).max(1)
    };
    let card_height = if height >= 600 {
        500.min(height.saturating_sub(48))
    } else {
        height.saturating_sub(24).max(1)
    };
    let card = crate::windowing::Rect::new(
        work_area.origin.x + (width - card_width) / 2,
        work_area.origin.y + (height - card_height) / 2,
        card_width,
        card_height,
    );
    let wide = card_width >= 580 && card_height >= 360;
    let pad = if wide { 30 } else { 18 };
    let illustration = if wide {
        crate::windowing::Rect::new(card.origin.x + pad, card.origin.y + 78, 190, 190)
    } else {
        crate::windowing::Rect::new(card.origin.x + pad, card.origin.y + 54, 120, 76)
    };
    let content_left = if wide {
        illustration.right() + 30
    } else {
        card.origin.x + pad
    };
    let content_top = if wide {
        card.origin.y + 74
    } else {
        illustration.bottom() + 14
    };
    let content_width = (card.right() - pad - content_left).max(1);
    let footer_y = card.bottom().saturating_sub(58);
    let next_width = 92.min(card_width.saturating_sub(pad * 2).max(1));
    let skip_width = 92.min(card_width.saturating_sub(pad * 2).max(1));
    let back_width = 80.min(card_width.saturating_sub(pad * 2).max(1));
    let gap = 10;
    let next_x = card.right().saturating_sub(pad).saturating_sub(next_width);
    let skip_x = next_x.saturating_sub(gap).saturating_sub(skip_width);
    let back_x = card.origin.x + pad;

    TutorialLayout {
        backdrop: crate::windowing::Rect::new(work_area.origin.x, work_area.origin.y, width, height),
        card,
        close_button: crate::windowing::Rect::new(
            card.right().saturating_sub(40),
            card.origin.y + 16,
            24,
            24,
        ),
        progress: crate::windowing::Rect::new(
            card.origin.x + pad,
            card.origin.y + 32,
            card_width - pad * 2,
            4,
        ),
        illustration,
        eyebrow: crate::windowing::Rect::new(content_left, content_top, content_width, 16),
        title: crate::windowing::Rect::new(content_left, content_top + 24, content_width, 32),
        body: crate::windowing::Rect::new(content_left, content_top + 70, content_width, 54),
        shortcut: crate::windowing::Rect::new(content_left, content_top + 136, content_width, 30),
        control_hint: crate::windowing::Rect::new(back_x, footer_y - 24, (skip_x - back_x - gap).max(1), 16),
        back_button: crate::windowing::Rect::new(back_x, footer_y, back_width, 34),
        skip_button: crate::windowing::Rect::new(skip_x, footer_y, skip_width, 34),
        next_button: crate::windowing::Rect::new(next_x, footer_y, next_width, 34),
    }
}

pub fn tutorial_control_hit(
    layout: &TutorialLayout,
    pointer: crate::windowing::Point,
) -> Option<TutorialControl> {
    [
        (layout.close_button, TutorialControl::Close),
        (layout.back_button, TutorialControl::Back),
        (layout.skip_button, TutorialControl::Skip),
        (layout.next_button, TutorialControl::Next),
    ]
    .into_iter()
    .find(|(rect, _)| rect.contains_point(pointer))
    .map(|(_, control)| control)
}

/// The release notes shown by the update card, edition-titled like macOS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseNotes {
    /// The marketing name of this edition, e.g. "Rouch Ocean".
    pub edition: &'static str,
    /// The semantic version string, e.g. "0.1.0".
    pub version: &'static str,
    /// Headline entries of this release, most important first.
    pub highlights: &'static [&'static str],
}

/// Rouch's own release notes for this build.
pub const fn this_release() -> ReleaseNotes {
    ReleaseNotes {
        edition: "Rouch Ocean",
        version: env!("CARGO_PKG_VERSION"),
        highlights: &[
            "macOS-style window chrome with traffic lights",
            "Liquid Glass dock with pointer magnification",
            "Live wallpaper and per-app transparency",
            "Interactive move, resize, minimize and fullscreen",
            "Welcome and release-notes experience",
        ],
    }
}

/// One moment of the first-run animation timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WelcomeFrame {
    /// 0.0..1.0 how much the Rouch mark has risen to its final place.
    pub mark_progress: f32,
    /// 0.0..1.0 wordmark opacity under the mark.
    pub wordmark_progress: f32,
    /// 0.0..1.0 how much the desktop behind has faded in.
    pub desktop_progress: f32,
    /// True when the whole sequence has finished and input resumes.
    pub finished: bool,
}

impl WelcomeFrame {
    /// Sample the timeline at `elapsed` since the welcome started.
    ///
    /// The choreography mirrors the macOS setup film: the mark rises first,
    /// the wordmark catches it, then everything settles into the desktop.
    pub fn elapsed(elapsed: Duration) -> Self {
        const MARK: Duration = Duration::from_millis(900);
        const WORDMARK: Duration = Duration::from_millis(700);
        const SETTLE: Duration = Duration::from_millis(900);

        let mark_start = Duration::ZERO;
        let mark_done = mark_start + MARK;
        let wordmark_start = mark_start + Duration::from_millis(350);
        let wordmark_done = wordmark_start + WORDMARK;
        let settle_start = wordmark_done;
        let settle_done = settle_start + SETTLE;

        let mark = phase(elapsed, mark_start, mark_done, ease_out);
        let wordmark = phase(elapsed, wordmark_start, wordmark_done, ease_out);
        let desktop = phase(elapsed, settle_start, settle_done, ease_out);
        let finished = elapsed >= settle_done;

        Self {
            mark_progress: mark,
            wordmark_progress: wordmark,
            desktop_progress: desktop,
            finished,
        }
    }
}

/// One moment of the update-card animation timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UpdateFrame {
    /// 0.0..1.0 card slide-and-settle from the right, macOS update-sheet feel.
    pub card_progress: f32,
    /// True once the card has settled; clicks pass to the card afterwards.
    pub finished: bool,
}

impl UpdateFrame {
    pub fn elapsed(elapsed: Duration) -> Self {
        const SLIDE: Duration = Duration::from_millis(550);
        let done = SLIDE;
        let t = phase(elapsed, Duration::ZERO, done, ease_out);
        Self {
            card_progress: t,
            finished: elapsed >= done,
        }
    }
}

/// Normalized progress through [start, end] mapped by `easing`.
fn phase(now: Duration, start: Duration, end: Duration, easing: fn(f32) -> f32) -> f32 {
    if now <= start {
        return 0.0;
    }
    if now >= end {
        return 1.0;
    }
    let span = end.as_secs_f32() - start.as_secs_f32();
    let raw = (now.as_secs_f32() - start.as_secs_f32()) / span;
    easing(raw.clamp(0.0, 1.0))
}

/// Smooth deceleration used across both timelines.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Geometry of the first-run welcome composition, in work-area coordinates.
///
/// The mark sits above centre (it rises toward there); the wordmark sits
/// below it; both fade with the desktop behind them.
pub struct FirstRunLayout {
    /// Final resting centre of the Rouch mark.
    pub mark_centre: crate::windowing::Point,
    /// Rectangle the wordmark occupies once fully faded in.
    pub wordmark_rect: crate::windowing::Rect,
}

/// Lay the first-run elements out over the work area.
pub fn first_run_layout(work_area: crate::windowing::Rect) -> FirstRunLayout {
    let centre_x = work_area.origin.x + work_area.size.width / 2;
    let centre_y = work_area.origin.y + work_area.size.height / 2;
    FirstRunLayout {
        mark_centre: crate::windowing::Point {
            x: centre_x,
            y: centre_y - 90,
        },
        wordmark_rect: crate::windowing::Rect::new(centre_x - 150, centre_y + 10, 300, 44),
    }
}

/// Geometry of the update card, in work-area coordinates.
///
/// macOS update cards sit centre-screen, slightly above the middle, with
/// the edition mark on the left half and notes on the right half.
pub struct UpdateLayout {
    /// The card's frame.
    pub card: crate::windowing::Rect,
    /// The square the edition mark occupies on the left half.
    pub mark: crate::windowing::Rect,
    /// The notes column on the right half, title above entries.
    pub title: crate::windowing::Rect,
    /// Vertical band the changelog entries are laid into.
    pub entries: crate::windowing::Rect,
    /// The dismiss ("Continue") button frame.
    pub button: crate::windowing::Rect,
}

/// Lay the update card out over the work area.
pub fn update_layout(work_area: crate::windowing::Rect) -> UpdateLayout {
    let width = 640.min(work_area.size.width - 80);
    let height = 400.min(work_area.size.height - 80);
    let left = work_area.origin.x + (work_area.size.width - width) / 2;
    let top = work_area.origin.y + (work_area.size.height - height) / 2;

    let mark_size = 128;
    UpdateLayout {
        card: crate::windowing::Rect::new(left, top, width, height),
        mark: crate::windowing::Rect::new(left + 40, top + (height - mark_size) / 2, mark_size, mark_size),
        title: crate::windowing::Rect::new(left + 40 + mark_size + 40, top + 64, width - mark_size - 160, 40),
        entries: crate::windowing::Rect::new(
            left + 40 + mark_size + 40,
            top + 118,
            width - mark_size - 160,
            height - 210,
        ),
        button: crate::windowing::Rect::new(left + width - 200, top + height - 72, 160, 44),
    }
}

/// True when a press lands on the update card's dismiss button.
pub fn update_button_hit(layout: &UpdateLayout, pointer: crate::windowing::Point) -> bool {
    layout.button.contains_point(pointer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_decision_covers_install_update_and_return() {
        assert_eq!(WelcomeStage::decide(None, "0.1.0"), WelcomeStage::FirstRun);
        assert_eq!(
            WelcomeStage::decide(Some("0.1.0"), "0.1.0"),
            WelcomeStage::Desktop
        );
        assert_eq!(WelcomeStage::decide(Some("0.0.9"), "0.1.0"), WelcomeStage::Update);
    }

    #[test]
    fn first_run_timeline_plays_in_order() {
        let early = WelcomeFrame::elapsed(Duration::from_millis(100));
        assert!(early.mark_progress > 0.0);
        assert_eq!(early.wordmark_progress, 0.0);
        assert_eq!(early.desktop_progress, 0.0);
        assert!(!early.finished);

        let mid = WelcomeFrame::elapsed(Duration::from_millis(900));
        assert!(mid.mark_progress > 0.5);
        assert!(mid.wordmark_progress > 0.0);
        assert_eq!(mid.desktop_progress, 0.0);

        let done = WelcomeFrame::elapsed(Duration::from_millis(4000));
        assert!(done.finished);
        assert_eq!(done.mark_progress, 1.0);
        assert_eq!(done.wordmark_progress, 1.0);
        assert_eq!(done.desktop_progress, 1.0);
    }

    #[test]
    fn update_card_slides_then_finishes() {
        let start = UpdateFrame::elapsed(Duration::ZERO);
        assert_eq!(start.card_progress, 0.0);
        assert!(!start.finished);

        let done = UpdateFrame::elapsed(Duration::from_millis(1200));
        assert!(done.finished);
        assert_eq!(done.card_progress, 1.0);
    }

    #[test]
    fn update_layout_is_centred_with_mark_left_and_notes_right() {
        let work_area = crate::windowing::Rect::new(0, 0, 1440, 900);
        let layout = update_layout(work_area);

        let card_centre_x = layout.card.origin.x + layout.card.size.width / 2;
        assert_eq!(card_centre_x, work_area.size.width / 2);

        assert!(layout.mark.right() < layout.title.origin.x);
        assert!(layout.title.origin.y < layout.entries.origin.y);
        assert!(layout.button.bottom() < layout.card.bottom());
        assert!(layout.button.right() <= layout.card.right());
    }

    #[test]
    fn dismiss_button_hit_testing_is_exact() {
        let layout = update_layout(crate::windowing::Rect::new(0, 0, 1440, 900));
        let centre = crate::windowing::Point {
            x: layout.button.origin.x + layout.button.size.width / 2,
            y: layout.button.origin.y + layout.button.size.height / 2,
        };
        assert!(update_button_hit(&layout, centre));

        let outside = crate::windowing::Point { x: 5, y: 5 };
        assert!(!update_button_hit(&layout, outside));
    }

    #[test]
    fn release_notes_carry_the_running_version() {
        let notes = this_release();
        assert_eq!(notes.version, env!("CARGO_PKG_VERSION"));
        assert!(!notes.highlights.is_empty());
        assert!(notes.edition.contains("Rouch"));
    }

    #[test]
    fn tutorial_covers_the_shell_in_a_short_linear_order() {
        assert_eq!(TUTORIAL_STEPS.len(), 6);
        assert_eq!(TutorialStep::first(), TutorialStep::TopBar);
        assert_eq!(TutorialStep::last(), TutorialStep::Terminal);
        assert_eq!(TutorialStep::TopBar.next(), Some(TutorialStep::Dock));
        assert_eq!(TutorialStep::Terminal.next(), None);

        let titles = TUTORIAL_STEPS
            .iter()
            .map(|step| step.content().title)
            .collect::<Vec<_>>();
        assert!(titles.iter().any(|title| title.contains("top bar")));
        assert!(titles.iter().any(|title| title.contains("Dock")));
        assert!(titles.iter().any(|title| title.contains("App Gallery")));
        assert!(titles.iter().any(|title| title.contains("notifications")));
        assert!(titles.iter().any(|title| title.contains("windows")));
        assert!(titles.iter().any(|title| title.contains("terminal")));
    }

    #[test]
    fn tutorial_focus_and_keyboard_actions_are_predictable() {
        let mut state = TutorialState::new();
        assert_eq!(state.focus, TutorialFocus::Next);
        state.apply(TutorialAction::FocusNext);
        assert_eq!(state.focus, TutorialFocus::Close);
        state.apply(TutorialAction::FocusNext);
        assert_eq!(state.focus, TutorialFocus::Back);
        state.apply(TutorialAction::FocusPrevious);
        assert_eq!(state.focus, TutorialFocus::Close);

        state.apply(TutorialAction::Restart);
        state.apply(TutorialKey::Enter.action());
        assert_eq!(state.step, TutorialStep::Dock);
        state.apply(TutorialKey::Escape.action());
        assert_eq!(state.status, TutorialStatus::Dismissed);
        assert!(!state.is_open());
    }

    #[test]
    fn tutorial_skip_preserves_page_and_resume_reopens_it() {
        let mut state = TutorialState::new();
        state.apply(TutorialAction::Next);
        state.apply(TutorialAction::Next);
        assert_eq!(state.step, TutorialStep::AppGallery);

        state.apply(TutorialAction::Skip);
        assert_eq!(state.status, TutorialStatus::Skipped);
        assert_eq!(state.step, TutorialStep::AppGallery);

        state.apply(TutorialAction::Resume);
        assert_eq!(state.status, TutorialStatus::Open);
        assert_eq!(state.step, TutorialStep::AppGallery);
    }

    #[test]
    fn tutorial_persistence_round_trip_is_bounded_and_resumable() {
        let mut state = TutorialState::new();
        state.apply(TutorialAction::Next);
        state.apply(TutorialAction::SetReducedMotion(true));
        state.apply(TutorialAction::FocusPrevious);
        let encoded = state.encode();
        let decoded = TutorialState::decode(&encoded).unwrap();
        assert_eq!(decoded, state);
        assert_eq!(
            default_tutorial_state_path(Path::new("/home/alice")),
            PathBuf::from("/home/alice/.local/state/rouch/tutorial.state")
        );

        let plan = state
            .persistence_plan(PathBuf::from("/home/alice/.local/state/rouch/tutorial.state"))
            .unwrap();
        assert!(plan.contents.starts_with(TUTORIAL_PERSISTENCE_HEADER));
        assert!(
            plan.temporary_path
                .to_string_lossy()
                .ends_with("tutorial.state.tmp")
        );
        assert!(matches!(
            TutorialState::decode(&"x".repeat(257)),
            Err(TutorialDecodeError::TooLarge)
        ));
    }

    #[test]
    fn tutorial_layout_has_keyboard_targets_and_safe_small_output() {
        let large = tutorial_layout(crate::windowing::Rect::new(0, 0, 1440, 900));
        assert!(large.card.size.width >= 580);
        assert!(large.card.contains_point(crate::windowing::Point {
            x: large.card.origin.x + large.card.size.width / 2,
            y: large.card.origin.y + large.card.size.height / 2,
        }));
        assert_eq!(
            tutorial_control_hit(
                &large,
                crate::windowing::Point {
                    x: large.next_button.origin.x + 4,
                    y: large.next_button.origin.y + 4,
                }
            ),
            Some(TutorialControl::Next)
        );

        let tiny = tutorial_layout(crate::windowing::Rect::new(0, 0, 12, 12));
        assert!(tiny.card.size.width > 0);
        assert!(tiny.card.size.height > 0);
        assert!(tiny.back_button.size.width > 0);
        assert!(tiny.next_button.size.width > 0);
    }
}
