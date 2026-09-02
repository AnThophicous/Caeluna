//! The Finder model: the pure file-browsing state the renderer samples.
//!
//! Everything here is data and rules only. The real filesystem backend
//! lives in `src/linux/finder_backend.rs` and the drawing in
//! `src/linux/finder_render.rs`; this module stays renderer-free and
//! platform-independent so its tests run from anywhere.

/// One entry in a Finder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinderItem {
    /// The display name, normally the file name.
    pub name: String,
    /// True for directories, which navigate instead of launching.
    pub directory: bool,
    /// The item's size in bytes.
    pub size_bytes: u64,
}

/// The listing sort orders the Finder offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FinderSort {
    /// Alphabetical, case-insensitive.
    #[default]
    Name,
    /// By size, largest first; folders stay ahead of files.
    Size,
    /// By modification date; alphabetical until mtimes are wired in.
    Date,
}

/// Width of the Finder's sidebar.
pub const SIDEBAR_WIDTH: i32 = 180;

/// Height of the toolbar band.
pub const TOOLBAR_HEIGHT: i32 = 40;

/// Width of each forgiving back/forward hit target in the toolbar.
pub const TOOLBAR_BUTTON_WIDTH: i32 = 40;

/// Vertical inset for toolbar navigation targets.
pub const TOOLBAR_BUTTON_INSET: i32 = 4;

/// One grid tile's icon square.
pub const TILE: i32 = 76;

/// Label height under each tile.
pub const LABEL: i32 = 20;

/// Gap between grid tiles.
pub const GAP: i32 = 14;

/// The Finder window's rectangle, centred on the work area.
pub fn window_rect(work_area: crate::windowing::Rect) -> crate::windowing::Rect {
    let width = 900.min(work_area.size.width - 32);
    let height = 560.min(work_area.size.height - 32);
    crate::windowing::Rect::new(
        work_area.origin.x + (work_area.size.width - width) / 2,
        work_area.origin.y + (work_area.size.height - height) / 2,
        width,
        height,
    )
}

/// The Finder's navigation state: a browser history with a filter query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinderState {
    /// Visited paths; the last is the current directory.
    pub history: Vec<String>,
    /// Paths kept for the forward button.
    pub forward: Vec<String>,
    /// The live search filter.
    pub query: String,
    /// The selected grid index, within the filtered view.
    pub selection: Option<usize>,
    /// The active sort.
    pub sort: FinderSort,
}

impl FinderState {
    /// Start browsing at `start`.
    pub fn new(start: &str) -> Self {
        Self {
            history: vec![start.to_owned()],
            forward: Vec::new(),
            query: String::new(),
            selection: None,
            sort: FinderSort::Name,
        }
    }

    /// The directory currently being browsed.
    pub fn current(&self) -> &str {
        self.history.last().map(String::as_str).unwrap_or("/")
    }

    /// Jump to a new directory; the forward stack is cleared.
    pub fn navigate(&mut self, path: &str) {
        if path == self.current() {
            return;
        }
        self.history.push(path.to_owned());
        self.forward.clear();
        self.selection = None;
    }

    /// Step back one directory. True when a step happened.
    pub fn back(&mut self) -> bool {
        if self.history.len() < 2 {
            return false;
        }
        let popped = self.history.pop().expect("checked len");
        self.forward.push(popped);
        self.selection = None;
        true
    }

    /// Step forward one directory. True when a step happened.
    pub fn forward_(&mut self) -> bool {
        match self.forward.pop() {
            Some(path) => {
                self.history.push(path);
                self.selection = None;
                true
            }
            None => false,
        }
    }

    /// The items visible after the search filter, case-insensitive.
    pub fn filtered<'a>(&self, items: &'a [FinderItem]) -> Vec<&'a FinderItem> {
        let query = self.query.trim().to_lowercase();
        items
            .iter()
            .filter(|item| query.is_empty() || item.name.to_lowercase().contains(&query))
            .collect()
    }
}

/// The window's interior regions: sidebar, toolbar, content and grid.
pub fn layout(
    window: crate::windowing::Rect,
) -> (
    crate::windowing::Rect,
    crate::windowing::Rect,
    crate::windowing::Rect,
    crate::windowing::Rect,
) {
    let sidebar = crate::windowing::Rect::new(
        window.origin.x,
        window.origin.y + TOOLBAR_HEIGHT,
        SIDEBAR_WIDTH,
        window.size.height - TOOLBAR_HEIGHT,
    );
    let toolbar = crate::windowing::Rect::new(
        window.origin.x,
        window.origin.y,
        window.size.width,
        TOOLBAR_HEIGHT,
    );
    let content = crate::windowing::Rect::new(
        sidebar.right(),
        window.origin.y + TOOLBAR_HEIGHT,
        (window.right() - sidebar.right()).max(0),
        window.size.height - TOOLBAR_HEIGHT,
    );
    let grid = crate::windowing::Rect::new(
        content.origin.x + GAP,
        content.origin.y + GAP,
        (content.size.width - 2 * GAP).max(0),
        (content.size.height - 2 * GAP).max(0),
    );
    (sidebar, toolbar, content, grid)
}

/// Resolve the Finder's back/forward buttons using the same adjacent slots
/// that the renderer paints.  Keeping these rectangles disjoint prevents an
/// edge click from activating the wrong direction.
pub fn toolbar_button_at(
    window: crate::windowing::Rect,
    toolbar: crate::windowing::Rect,
    point: crate::windowing::Point,
) -> Option<usize> {
    (0..2_usize).find(|index| {
        crate::windowing::Rect::new(
            window.origin.x + 14 + *index as i32 * TOOLBAR_BUTTON_WIDTH,
            toolbar.origin.y + TOOLBAR_BUTTON_INSET,
            TOOLBAR_BUTTON_WIDTH,
            (toolbar.size.height - 2 * TOOLBAR_BUTTON_INSET).max(0),
        )
        .contains_point(point)
    })
}

/// The tile rect of one grid index, flowing in columns.
pub fn item_rect(grid: crate::windowing::Rect, index: usize) -> crate::windowing::Rect {
    let cell = TILE + LABEL + GAP;
    let per_row = (grid.size.width / (TILE + GAP)).max(1) as usize;
    let column = index % per_row;
    let row = index / per_row;
    crate::windowing::Rect::new(
        grid.origin.x + column as i32 * (TILE + GAP),
        grid.origin.y + row as i32 * cell,
        TILE,
        TILE + LABEL,
    )
}

/// Which grid tile a point hits, if any.
pub fn item_at(grid: crate::windowing::Rect, count: usize, point: crate::windowing::Point) -> Option<usize> {
    (0..count)
        .map(|index| item_rect(grid, index))
        .position(|rect| rect.contains_point(point))
}

/// The sidebar's favourite places: label and target path.
pub fn sidebar_places() -> Vec<(&'static str, String)> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
    vec![
        ("Recents", home.clone()),
        ("Applications", "/usr/share/applications".to_owned()),
        ("Desktop", format!("{home}/Desktop")),
        ("Documents", format!("{home}/Documents")),
        ("Downloads", format!("{home}/Downloads")),
        ("Pictures", format!("{home}/Pictures")),
        ("Home", home),
    ]
}

/// Which sidebar row a point hits, if any.
pub fn sidebar_hit(sidebar: crate::windowing::Rect, point: crate::windowing::Point) -> Option<usize> {
    if !sidebar.contains_point(point) {
        return None;
    }
    let row_height = 34;
    let index = (point.y - sidebar.origin.y) / row_height;
    if index < 0 {
        return None;
    }
    let places = sidebar_places();
    let index = index as usize;
    places.get(index).map(|_| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windowing::{Point, Rect};

    #[test]
    fn navigation_behaves_like_a_browser() {
        let mut state = FinderState::new("/home");
        state.navigate("/home/Documents");
        state.navigate("/tmp");
        assert_eq!(state.current(), "/tmp");

        assert!(state.back());
        assert_eq!(state.current(), "/home/Documents");
        assert!(state.back());
        assert_eq!(state.current(), "/home");
        assert!(!state.back());

        assert!(state.forward_());
        assert_eq!(state.current(), "/home/Documents");
        assert!(state.forward_());
        assert_eq!(state.current(), "/tmp");
        assert!(!state.forward_());
    }

    #[test]
    fn navigating_clears_the_forward_stack() {
        let mut state = FinderState::new("/home");
        state.navigate("/a");
        state.navigate("/b");
        assert!(state.back());
        state.navigate("/c");
        assert!(state.forward.is_empty());
        assert_eq!(state.current(), "/c");
    }

    #[test]
    fn filter_is_substring_case_insensitive() {
        let state = FinderState::new("/");
        let items = vec![
            FinderItem {
                name: "Report.pdf".into(),
                directory: false,
                size_bytes: 10,
            },
            FinderItem {
                name: "notes.txt".into(),
                directory: false,
                size_bytes: 5,
            },
        ];
        let query_state = FinderState {
            query: "REP".into(),
            ..state.clone()
        };
        assert_eq!(query_state.filtered(&items).len(), 1);
        assert_eq!(state.filtered(&items).len(), 2);
    }

    #[test]
    fn grid_tiles_never_overlap() {
        let grid = Rect::new(0, 0, 400, 600);
        let rects: Vec<Rect> = (0..12).map(|index| item_rect(grid, index)).collect();
        for (i, a) in rects.iter().enumerate() {
            for b in rects.iter().skip(i + 1) {
                let separated = a.right() <= b.origin.x
                    || b.right() <= a.origin.x
                    || a.bottom() <= b.origin.y
                    || b.bottom() <= a.origin.y;
                assert!(separated, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn grid_and_sidebar_hit_testing() {
        let grid = Rect::new(0, 0, 400, 600);
        let second = item_rect(grid, 1);
        let centre = Point {
            x: second.origin.x + TILE / 2,
            y: second.origin.y + TILE / 2,
        };
        assert_eq!(item_at(grid, 10, centre), Some(1));
        assert_eq!(item_at(grid, 10, Point { x: 5000, y: 5000 }), None);

        let sidebar = Rect::new(0, 40, SIDEBAR_WIDTH, 500);
        let second_row = Point {
            x: 20,
            y: sidebar.origin.y + 34 + 8,
        };
        assert_eq!(sidebar_hit(sidebar, second_row), Some(1));
        assert_eq!(sidebar_hit(sidebar, Point { x: 500, y: 60 }), None);
    }

    #[test]
    fn toolbar_navigation_targets_are_adjacent_not_overlapping() {
        let window = window_rect(Rect::new(0, 0, 1440, 900));
        let (_, toolbar, _, _) = layout(window);
        let back = Point {
            x: window.origin.x + 20,
            y: toolbar.origin.y + toolbar.size.height / 2,
        };
        let forward = Point {
            x: window.origin.x + 60,
            y: toolbar.origin.y + toolbar.size.height / 2,
        };
        assert_eq!(toolbar_button_at(window, toolbar, back), Some(0));
        assert_eq!(toolbar_button_at(window, toolbar, forward), Some(1));
        // The shared boundary belongs to the forward slot, never both.
        assert_eq!(
            toolbar_button_at(
                window,
                toolbar,
                Point {
                    x: window.origin.x + 54,
                    y: toolbar.origin.y + toolbar.size.height / 2,
                }
            ),
            Some(1)
        );
    }

    #[test]
    fn window_is_centred_and_clamped() {
        let work_area = Rect::new(0, 0, 1440, 900);
        let window = window_rect(work_area);
        let centre = window.origin.x + window.size.width / 2;
        assert_eq!(centre, work_area.size.width / 2);
        assert!(window.bottom() < work_area.bottom());

        let tiny = window_rect(Rect::new(0, 0, 400, 300));
        assert!(tiny.size.width <= 400);
        assert!(tiny.size.height <= 300);
    }
}
