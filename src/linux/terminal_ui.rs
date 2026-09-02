//! Pure UI state and geometry for the built-in terminal.
//!
//! The process/PTY implementation lives in `crate::terminal`. This module
//! deliberately owns only the compositor-facing state: tab identity, title
//! editing, per-tab material choices and bounded visible output. Keeping the
//! model independent from Smithay makes hit testing deterministic and gives
//! the renderer one small, allocation-conscious input surface.

use crate::windowing::{Point, Rect};

/// The terminal window is intentionally smaller than the whole output so it
/// reads as an app window and leaves the Tahoe shell visible around it.
pub const WINDOW_WIDTH: i32 = 960;
pub const WINDOW_HEIGHT: i32 = 620;
pub const MIN_WINDOW_WIDTH: i32 = 480;
pub const MIN_WINDOW_HEIGHT: i32 = 300;
pub const TITLE_BAR_HEIGHT: i32 = 42;
pub const TAB_BAR_HEIGHT: i32 = 38;
pub const STATUS_BAR_HEIGHT: i32 = 24;
pub const TERMINAL_PADDING: i32 = 20;
pub const MAX_TABS: usize = 8;
pub const MAX_TITLE_CHARS: usize = 48;
pub const MAX_VISIBLE_LINES: usize = 240;
pub const MAX_LINE_CHARS: usize = 8_192;

/// Four restrained backgrounds are enough to personalize tabs without
/// turning the terminal into a second theme engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalTheme {
    Ocean,
    Graphite,
    Paper,
    Night,
}

impl TerminalTheme {
    pub const ALL: [Self; 4] = [Self::Ocean, Self::Graphite, Self::Paper, Self::Night];

    pub const fn next(self) -> Self {
        match self {
            Self::Ocean => Self::Graphite,
            Self::Graphite => Self::Paper,
            Self::Paper => Self::Night,
            Self::Night => Self::Ocean,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ocean => "Ocean",
            Self::Graphite => "Graphite",
            Self::Paper => "Paper",
            Self::Night => "Night",
        }
    }

    /// BGRA colours used by the renderer. Content backgrounds stay opaque;
    /// only the tab/title chrome may receive an alpha value from the material
    /// policy.
    pub const fn palette(self) -> TerminalPalette {
        match self {
            Self::Ocean => TerminalPalette {
                background: [12, 27, 50, 255],
                panel: [29, 53, 82, 224],
                panel_selected: [50, 83, 118, 242],
                text: [235, 244, 252, 255],
                muted: [158, 183, 207, 255],
                accent: [106, 204, 242, 255],
                cursor: [151, 221, 248, 255],
                status: [20, 42, 67, 255],
            },
            Self::Graphite => TerminalPalette {
                background: [25, 26, 30, 255],
                panel: [49, 50, 56, 224],
                panel_selected: [76, 78, 88, 242],
                text: [245, 246, 248, 255],
                muted: [177, 180, 188, 255],
                accent: [143, 194, 255, 255],
                cursor: [184, 215, 255, 255],
                status: [36, 37, 43, 255],
            },
            Self::Paper => TerminalPalette {
                background: [241, 242, 239, 255],
                panel: [225, 227, 224, 240],
                panel_selected: [202, 220, 233, 248],
                text: [28, 34, 40, 255],
                muted: [88, 97, 104, 255],
                accent: [17, 104, 152, 255],
                cursor: [24, 122, 165, 255],
                status: [213, 216, 212, 255],
            },
            Self::Night => TerminalPalette {
                background: [7, 10, 18, 255],
                panel: [25, 31, 47, 224],
                panel_selected: [41, 55, 80, 242],
                text: [231, 238, 251, 255],
                muted: [142, 159, 183, 255],
                accent: [141, 180, 255, 255],
                cursor: [170, 201, 255, 255],
                status: [15, 20, 33, 255],
            },
        }
    }
}

/// A renderer-ready palette for one terminal tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalPalette {
    pub background: [u8; 4],
    pub panel: [u8; 4],
    pub panel_selected: [u8; 4],
    pub text: [u8; 4],
    pub muted: [u8; 4],
    pub accent: [u8; 4],
    pub cursor: [u8; 4],
    pub status: [u8; 4],
}

/// A single tab's UI projection of a real PTY session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTab {
    pub id: u64,
    pub title: String,
    pub theme: TerminalTheme,
    pub transparency: bool,
    pub blur: bool,
    pub cwd: String,
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_column: usize,
    pub cursor_visible: bool,
    pub alive: bool,
}

impl TerminalTab {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            title: "Terminal".into(),
            theme: TerminalTheme::Ocean,
            transparency: true,
            blur: true,
            cwd: "~".into(),
            lines: Vec::new(),
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: true,
            alive: true,
        }
    }

    /// Replace the visible projection from the real terminal core. Bounds
    /// are enforced here as a second line of defence against an untrusted or
    /// noisy PTY stream causing an oversized UI allocation.
    pub fn set_snapshot(
        &mut self,
        title: Option<&str>,
        cwd: Option<&str>,
        lines: &[String],
        cursor_row: usize,
        cursor_column: usize,
        cursor_visible: bool,
    ) {
        if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
            self.title = clamp_text(title, MAX_TITLE_CHARS);
        }
        if let Some(cwd) = cwd.filter(|value| !value.trim().is_empty()) {
            self.cwd = clamp_text(cwd, 96);
        }
        self.lines.clear();
        let start = lines.len().saturating_sub(MAX_VISIBLE_LINES);
        self.lines
            .extend(lines[start..].iter().map(|line| clamp_text(line, MAX_LINE_CHARS)));
        self.cursor_row = cursor_row.min(self.lines.len().saturating_sub(1));
        self.cursor_column = cursor_column.min(MAX_LINE_CHARS);
        self.cursor_visible = cursor_visible;
        self.alive = true;
    }

    pub fn mark_exited(&mut self, title: Option<&str>) {
        if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
            self.title = clamp_text(title, MAX_TITLE_CHARS);
        }
        self.alive = false;
        self.cursor_visible = false;
    }
}

/// The visible terminal surface and its keyboard-editing state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalUi {
    pub open: bool,
    pub focused: bool,
    pub tabs: Vec<TerminalTab>,
    pub active_tab: usize,
    pub title_editing: bool,
    pub title_buffer: String,
    pub reduced_motion: bool,
    pub reduce_transparency: bool,
    next_tab_id: u64,
    revision: u64,
}

impl Default for TerminalUi {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalUi {
    pub fn new() -> Self {
        Self {
            open: false,
            focused: false,
            tabs: Vec::new(),
            active_tab: 0,
            title_editing: false,
            title_buffer: String::new(),
            reduced_motion: false,
            reduce_transparency: false,
            next_tab_id: 1,
            revision: 1,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Open the terminal and create the first real session slot exactly once.
    /// The caller consumes the returned tab id to ask `crate::terminal` to
    /// spawn its PTY; no command is fabricated by this UI layer.
    pub fn open(&mut self) -> Option<u64> {
        let first = self.tabs.is_empty();
        self.open = true;
        self.focused = true;
        self.title_editing = false;
        if first {
            let id = self.allocate_tab();
            self.changed();
            Some(id)
        } else {
            self.changed();
            None
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.focused = false;
        self.title_editing = false;
        self.changed();
    }

    /// Toggle visibility. A newly opened terminal reports the id that needs a
    /// PTY session; an already populated terminal only changes focus.
    pub fn toggle(&mut self) -> Option<u64> {
        if self.open {
            self.close();
            None
        } else {
            self.open()
        }
    }

    pub fn focus(&mut self) {
        self.open = true;
        self.focused = true;
        self.changed();
    }

    pub fn blur_focus(&mut self) {
        self.focused = false;
        self.title_editing = false;
        self.changed();
    }

    /// Start a new tab and return its stable session id. The tab count is
    /// bounded so an accidental shortcut repeat cannot allocate indefinitely.
    pub fn new_tab(&mut self) -> Option<u64> {
        if self.tabs.len() >= MAX_TABS {
            return None;
        }
        let id = self.allocate_tab();
        self.open = true;
        self.focused = true;
        self.changed();
        Some(id)
    }

    fn allocate_tab(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.wrapping_add(1).max(1);
        self.tabs.push(TerminalTab::new(id));
        self.active_tab = self.tabs.len() - 1;
        id
    }

    /// Close a tab. Closing the last tab closes the terminal surface, like a
    /// native terminal window, and lets the core reap that session.
    pub fn close_tab(&mut self, index: usize) -> Option<u64> {
        if index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.close();
        } else {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
            self.title_editing = false;
            self.changed();
        }
        Some(tab.id)
    }

    pub fn active_tab(&self) -> Option<&TerminalTab> {
        self.tabs.get(self.active_tab)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut TerminalTab> {
        self.tabs.get_mut(self.active_tab)
    }

    pub fn select_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active_tab {
            return false;
        }
        self.active_tab = index;
        self.title_editing = false;
        self.changed();
        true
    }

    pub fn select_next_tab(&mut self, reverse: bool) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        let next = if reverse {
            self.active_tab.checked_sub(1).unwrap_or(self.tabs.len() - 1)
        } else {
            (self.active_tab + 1) % self.tabs.len()
        };
        self.select_tab(next)
    }

    pub fn begin_title_edit(&mut self) -> bool {
        let Some(tab) = self.active_tab() else {
            return false;
        };
        self.title_buffer = tab.title.clone();
        self.title_editing = true;
        self.changed();
        true
    }

    pub fn title_type(&mut self, text: &str) {
        if !self.title_editing {
            return;
        }
        self.title_buffer.push_str(text);
        self.title_buffer = clamp_text(&self.title_buffer, MAX_TITLE_CHARS);
        self.changed();
    }

    pub fn title_backspace(&mut self) {
        if self.title_editing {
            self.title_buffer.pop();
            self.changed();
        }
    }

    pub fn commit_title(&mut self) -> bool {
        if !self.title_editing {
            return false;
        }
        let title = if self.title_buffer.trim().is_empty() {
            "Terminal".into()
        } else {
            clamp_text(&self.title_buffer, MAX_TITLE_CHARS)
        };
        let Some(tab) = self.active_tab_mut() else {
            self.title_editing = false;
            self.changed();
            return false;
        };
        let changed = tab.title != title;
        tab.title = title;
        self.title_editing = false;
        self.title_buffer.clear();
        self.changed();
        changed
    }

    pub fn cancel_title_edit(&mut self) {
        if self.title_editing {
            self.title_editing = false;
            self.title_buffer.clear();
            self.changed();
        }
    }

    pub fn cycle_theme(&mut self) -> bool {
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        tab.theme = tab.theme.next();
        self.changed();
        true
    }

    pub fn toggle_tab_transparency(&mut self) -> bool {
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        tab.transparency = !tab.transparency;
        self.changed();
        true
    }

    pub fn toggle_tab_blur(&mut self) -> bool {
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        tab.blur = !tab.blur;
        self.changed();
        true
    }

    pub fn set_snapshot(
        &mut self,
        id: u64,
        title: Option<&str>,
        cwd: Option<&str>,
        lines: &[String],
        cursor_row: usize,
        cursor_column: usize,
        cursor_visible: bool,
    ) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return false;
        };
        tab.set_snapshot(title, cwd, lines, cursor_row, cursor_column, cursor_visible);
        self.changed();
        true
    }

    pub fn mark_session_exited(&mut self, id: u64, title: Option<&str>) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return false;
        };
        tab.mark_exited(title);
        self.changed();
        true
    }

    pub fn layout(&self, work_area: Rect) -> TerminalLayout {
        layout(work_area, self.tabs.len())
    }

    pub fn hit_test(&self, work_area: Rect, point: Point) -> TerminalHit {
        if !self.open {
            return TerminalHit::None;
        }
        self.layout(work_area).hit_test(point)
    }
}

/// Geometry sampled by the renderer and input adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLayout {
    pub window: Rect,
    pub title_bar: Rect,
    pub tab_bar: Rect,
    pub content: Rect,
    pub status_bar: Rect,
    pub title_edit: Rect,
    pub new_tab: Rect,
    pub theme: Rect,
    pub transparency: Rect,
    pub blur: Rect,
    pub tabs: Vec<TabGeometry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabGeometry {
    pub tab: Rect,
    pub close: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalHit {
    None,
    Window,
    Title,
    TitleEdit,
    NewTab,
    Tab(usize),
    CloseTab(usize),
    Theme,
    Transparency,
    Blur,
    Content,
}

pub fn window_rect(work_area: Rect) -> Rect {
    let width = WINDOW_WIDTH
        .min(work_area.size.width.saturating_sub(32))
        .max(MIN_WINDOW_WIDTH.min(work_area.size.width.max(1)));
    let height = WINDOW_HEIGHT
        .min(work_area.size.height.saturating_sub(32))
        .max(MIN_WINDOW_HEIGHT.min(work_area.size.height.max(1)));
    Rect::new(
        work_area.origin.x + (work_area.size.width.saturating_sub(width)) / 2,
        work_area.origin.y + (work_area.size.height.saturating_sub(height)) / 2,
        width.min(work_area.size.width.max(1)),
        height.min(work_area.size.height.max(1)),
    )
}

pub fn layout(work_area: Rect, tab_count: usize) -> TerminalLayout {
    let window = window_rect(work_area);
    let title_bar = Rect::new(
        window.origin.x,
        window.origin.y,
        window.size.width,
        TITLE_BAR_HEIGHT,
    );
    let tab_bar = Rect::new(
        window.origin.x,
        title_bar.bottom(),
        window.size.width,
        TAB_BAR_HEIGHT,
    );
    let status_bar = Rect::new(
        window.origin.x,
        window.bottom().saturating_sub(STATUS_BAR_HEIGHT),
        window.size.width,
        STATUS_BAR_HEIGHT,
    );
    let content = Rect::new(
        window.origin.x,
        tab_bar.bottom(),
        window.size.width,
        status_bar.origin.y.saturating_sub(tab_bar.bottom()),
    );
    let title_edit = Rect::new(window.origin.x + 128, window.origin.y + 7, 260, 28);
    let new_tab = Rect::new(window.right().saturating_sub(44), tab_bar.origin.y + 7, 30, 24);
    let theme = Rect::new(window.right().saturating_sub(142), tab_bar.origin.y + 7, 26, 24);
    let transparency = Rect::new(window.right().saturating_sub(110), tab_bar.origin.y + 7, 26, 24);
    let blur = Rect::new(window.right().saturating_sub(78), tab_bar.origin.y + 7, 26, 24);

    let left = tab_bar.origin.x + 118;
    let right = theme.origin.x.saturating_sub(12);
    let available = right.saturating_sub(left);
    let count = tab_count.max(1);
    let tab_width = (available / count as i32).clamp(132, 220);
    let tabs = (0..tab_count)
        .map(|index| {
            let x = left + index as i32 * tab_width;
            let tab = Rect::new(x, tab_bar.origin.y + 5, tab_width.saturating_sub(6), 28);
            let close = Rect::new(tab.right().saturating_sub(28), tab.origin.y + 5, 20, 18);
            TabGeometry { tab, close }
        })
        .collect();

    TerminalLayout {
        window,
        title_bar,
        tab_bar,
        content,
        status_bar,
        title_edit,
        new_tab,
        theme,
        transparency,
        blur,
        tabs,
    }
}

impl TerminalLayout {
    pub fn hit_test(&self, point: Point) -> TerminalHit {
        if self.new_tab.contains_point(point) {
            return TerminalHit::NewTab;
        }
        if self.theme.contains_point(point) {
            return TerminalHit::Theme;
        }
        if self.transparency.contains_point(point) {
            return TerminalHit::Transparency;
        }
        if self.blur.contains_point(point) {
            return TerminalHit::Blur;
        }
        if self.title_edit.contains_point(point) {
            return TerminalHit::TitleEdit;
        }
        if let Some((index, tab)) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(_, tab)| tab.close.contains_point(point))
        {
            return TerminalHit::CloseTab(index);
        }
        if let Some((index, tab)) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(_, tab)| tab.tab.contains_point(point))
        {
            return TerminalHit::Tab(index);
        }
        if self.title_bar.contains_point(point) {
            return TerminalHit::Title;
        }
        if self.content.contains_point(point) {
            return TerminalHit::Content;
        }
        if self.window.contains_point(point) {
            return TerminalHit::Window;
        }
        TerminalHit::None
    }
}

fn clamp_text(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

    #[test]
    fn first_open_returns_one_session_request_and_reuses_it() {
        let mut ui = TerminalUi::new();
        let first = ui.open();
        assert_eq!(first, Some(1));
        assert_eq!(ui.open(), None);
        assert_eq!(ui.tabs.len(), 1);
        assert_eq!(ui.active_tab().map(|tab| tab.id), Some(1));
    }

    #[test]
    fn tabs_are_bounded_and_cycle_in_both_directions() {
        let mut ui = TerminalUi::new();
        assert_eq!(ui.open(), Some(1));
        for _ in 1..MAX_TABS {
            assert!(ui.new_tab().is_some());
        }
        assert!(ui.new_tab().is_none());
        assert!(ui.select_next_tab(false));
        assert!(ui.select_next_tab(true));
        assert_eq!(ui.tabs.len(), MAX_TABS);
    }

    #[test]
    fn closing_last_tab_closes_surface_and_returns_session_id() {
        let mut ui = TerminalUi::new();
        assert_eq!(ui.open(), Some(1));
        assert_eq!(ui.close_tab(0), Some(1));
        assert!(!ui.open);
        assert!(ui.tabs.is_empty());
    }

    #[test]
    fn title_editing_is_bounded_and_cancelable() {
        let mut ui = TerminalUi::new();
        ui.open();
        assert!(ui.begin_title_edit());
        ui.title_type(&"x".repeat(MAX_TITLE_CHARS + 12));
        assert_eq!(ui.title_buffer.chars().count(), MAX_TITLE_CHARS);
        ui.cancel_title_edit();
        assert!(!ui.title_editing);
        assert_eq!(ui.active_tab().map(|tab| tab.title.as_str()), Some("Terminal"));
    }

    #[test]
    fn snapshot_is_bounded_and_replaces_visible_projection() {
        let mut ui = TerminalUi::new();
        ui.open();
        let lines = (0..MAX_VISIBLE_LINES + 5)
            .map(|index| format!("{index}: {}", "x".repeat(MAX_LINE_CHARS + 4)))
            .collect::<Vec<_>>();
        assert!(ui.set_snapshot(1, Some("Shell"), Some("/tmp"), &lines, 999, 999, true));
        let tab = ui.active_tab().unwrap();
        assert_eq!(tab.lines.len(), MAX_VISIBLE_LINES);
        assert!(
            tab.lines
                .iter()
                .all(|line| line.chars().count() <= MAX_LINE_CHARS)
        );
        assert_eq!(tab.title, "Shell");
        assert_eq!(tab.cwd, "/tmp");
        assert_eq!(tab.cursor_row, MAX_VISIBLE_LINES - 1);
        assert_eq!(tab.cursor_column, MAX_LINE_CHARS);
    }

    #[test]
    fn layout_hit_targets_are_disjoint_and_clamped() {
        let layout = layout(WORK_AREA, 2);
        assert!(layout.window.size.width <= WORK_AREA.size.width);
        assert!(layout.window.size.height <= WORK_AREA.size.height);
        assert_eq!(
            layout.hit_test(Point::new(
                layout.new_tab.origin.x + 1,
                layout.new_tab.origin.y + 1
            )),
            TerminalHit::NewTab
        );
        assert_eq!(
            layout.hit_test(Point::new(layout.theme.origin.x + 1, layout.theme.origin.y + 1)),
            TerminalHit::Theme
        );
        assert_eq!(
            layout.hit_test(Point::new(
                layout.content.origin.x + 1,
                layout.content.origin.y + 1
            )),
            TerminalHit::Content
        );
        assert_ne!(layout.tabs[0].tab, layout.tabs[1].tab);
    }

    #[test]
    fn themes_cycle_without_allocating_or_using_unbounded_names() {
        let mut theme = TerminalTheme::Ocean;
        for expected in [
            TerminalTheme::Graphite,
            TerminalTheme::Paper,
            TerminalTheme::Night,
            TerminalTheme::Ocean,
        ] {
            theme = theme.next();
            assert_eq!(theme, expected);
        }
        assert_eq!(TerminalTheme::Ocean.palette().background[3], 255);
    }
}
