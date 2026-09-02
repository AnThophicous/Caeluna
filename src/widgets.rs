//! macOS Tahoe-style widgets: a right-rail board with live tiles, the
//! iPhone-style edit mode, and the classic "remove widget?" confirmation.
//!
//! The board itself is pure state and geometry. Long-pressing a widget
//! enters edit mode: every widget gains a corner minus button (Rouch keeps
//! the tiles steady â€” no jitter â€” but the interaction is otherwise the
//! Cupertino one). Tapping the minus raises the confirmation popup;
//! confirming removes the widget for good.

use std::time::Duration;

use crate::windowing::{Point, Rect, Size};

/// Width of one widget column.
pub const COLUMN_WIDTH: i32 = 180;

/// Gap between widgets.
pub const GAP: i32 = 14;

/// Right margin of the board from the work-area edge.
pub const BOARD_MARGIN: i32 = 12;

/// Radius of every widget tile.
pub const TILE_RADIUS: i32 = 20;

/// Radius of the corner minus button.
pub const MINUS_RADIUS: i32 = 11;

/// How long a press-and-hold must last to enter edit mode.
pub const HOLD_THRESHOLD: Duration = Duration::from_millis(450);

/// The widget kinds Rouch ships, mirroring the Tahoe set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidgetKind {
    /// Battery charge with charging state.
    Battery,
    /// Screen brightness slider.
    Brightness,
    /// Output volume slider.
    Volume,
    /// Network name and signal.
    Network,
    /// Date and time.
    Clock,
    /// Disk usage for the root filesystem.
    Storage,
    /// CPU load percent.
    Cpu,
    /// RAM usage percent.
    Memory,
}

impl WidgetKind {
    /// Display name, used by the confirmation popup.
    pub fn name(self) -> &'static str {
        match self {
            Self::Battery => "Battery",
            Self::Brightness => "Brightness",
            Self::Volume => "Volume",
            Self::Network => "Network",
            Self::Clock => "Clock",
            Self::Storage => "Storage",
            Self::Cpu => "CPU Load",
            Self::Memory => "Memory",
        }
    }

    /// The widget's default size class: small tiles are one column wide.
    pub fn is_small(self) -> bool {
        !matches!(self, Self::Clock)
    }

    /// The default board, Tahoe-arranged.
    pub fn default_board() -> Vec<Self> {
        vec![
            Self::Battery,
            Self::Clock,
            Self::Network,
            Self::Brightness,
            Self::Volume,
            Self::Cpu,
            Self::Memory,
            Self::Storage,
        ]
    }
}

/// One placed widget on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetSlot {
    pub kind: WidgetKind,
    pub column: usize,
    pub row: usize,
    /// Small widgets span one column; wide ones span two.
    pub spans_two: bool,
}

impl WidgetSlot {
    #[allow(dead_code)]
    fn from_kind(kind: WidgetKind) -> Self {
        Self {
            kind,
            column: 0,
            row: 0,
            spans_two: !kind.is_small(),
        }
    }

    /// The tile's rectangle for its place on the board.
    pub fn rect(self, board: Rect) -> Rect {
        let width = if self.spans_two {
            2 * COLUMN_WIDTH + GAP
        } else {
            COLUMN_WIDTH
        };
        let height = 180;
        Rect::new(
            board.origin.x + self.column as i32 * (COLUMN_WIDTH + GAP),
            board.origin.y + self.row as i32 * (height + GAP),
            width,
            height,
        )
    }
}

/// The board state: which widgets exist and whether edit mode is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetBoard {
    pub slots: Vec<WidgetSlot>,
    pub editing: bool,
}

impl Default for WidgetBoard {
    fn default() -> Self {
        Self::from_kinds(WidgetKind::default_board())
    }
}

impl WidgetBoard {
    /// Build a board from kinds, flowing them into a two-column grid.
    pub fn from_kinds(kinds: Vec<WidgetKind>) -> Self {
        let mut slots: Vec<WidgetSlot> = Vec::new();
        let mut column = 0;
        let mut row = 0;
        for kind in kinds {
            let slot = WidgetSlot {
                kind,
                column,
                row,
                spans_two: !kind.is_small(),
            };
            slots.push(slot);
            if slot.spans_two {
                column = 0;
                row += 1;
            } else if column == 0 {
                column = 1;
            } else {
                column = 0;
                row += 1;
            }
        }
        Self {
            slots,
            editing: false,
        }
    }

    /// Enter or leave edit mode.
    pub fn set_editing(&mut self, editing: bool) {
        self.editing = editing;
    }

    /// The widget a point lands on, if any.
    pub fn widget_at(&self, board: Rect, point: Point) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.rect(board).contains_point(point))
    }

    /// The corner minus button's centre for one widget.
    pub fn minus_center(&self, board: Rect, index: usize) -> Option<Point> {
        let slot = self.slots.get(index)?;
        let rect = slot.rect(board);
        Some(Point {
            x: rect.origin.x + 4,
            y: rect.origin.y + 4,
        })
    }

    /// Whether a point hits the minus button of a widget.
    pub fn minus_hit(&self, board: Rect, index: usize, point: Point) -> bool {
        let Some(rect) = self.slots.get(index).map(|slot| slot.rect(board)) else {
            return false;
        };
        let cx = rect.origin.x + 4;
        let cy = rect.origin.y + 4;
        let dx = point.x - cx;
        let dy = point.y - cy;
        dx * dx + dy * dy <= MINUS_RADIUS * MINUS_RADIUS
    }

    /// Remove a widget from the board, reflowing the remaining ones.
    pub fn remove(&mut self, index: usize) -> Option<WidgetKind> {
        if index >= self.slots.len() {
            return None;
        }
        let removed = self.slots.remove(index).kind;
        let kinds: Vec<WidgetKind> = self.slots.iter().map(|slot| slot.kind).collect();
        let editing = self.editing;
        *self = Self::from_kinds(kinds);
        self.editing = editing;
        Some(removed)
    }

    /// The board's bounding rect for a work area. It hugs the right edge,
    /// below the top bar, like the Tahoe widget rail.
    pub fn board_rect(work_area: Rect) -> Rect {
        let rows = 4;
        let width = 2 * COLUMN_WIDTH + GAP;
        let height = rows * (180 + GAP);
        Rect::new(
            work_area.right() - BOARD_MARGIN - width,
            crate::topbar::BAR_HEIGHT + BOARD_MARGIN,
            width,
            height,
        )
    }
}

/// The confirmation popup state for removing a widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovePopup {
    /// The widget kind the popup asks about.
    pub kind: WidgetKind,
    /// Its slot index at the moment the popup opened.
    pub index: usize,
}

/// Layout of the confirmation popup, centred under its widget.
pub fn popup_rect(anchor: Rect, work_area: Rect) -> Rect {
    let width = 280;
    let height = 130;
    let centre_x = anchor.origin.x + anchor.size.width / 2;
    let x = (centre_x - width / 2).clamp(work_area.origin.x + 8, work_area.right() - width - 8);
    Rect::new(x, anchor.bottom() + 10, width, height)
}

/// The popup's Cancel and Remove buttons.
pub fn popup_buttons(popup: Rect) -> (Rect, Rect) {
    let cancel = Rect::new(popup.origin.x + 16, popup.bottom() - 56, 118, 40);
    let remove = Rect::new(popup.right() - 134, popup.bottom() - 56, 118, 40);
    (cancel, remove)
}

/// Hit-test the popup's buttons.
pub enum PopupHit {
    Cancel,
    Remove,
    Outside,
}

pub fn popup_hit(popup: Rect, point: Point) -> PopupHit {
    let (cancel, remove) = popup_buttons(popup);
    if cancel.contains_point(point) {
        PopupHit::Cancel
    } else if remove.contains_point(point) {
        PopupHit::Remove
    } else {
        PopupHit::Outside
    }
}

/// A live reading for one widget kind, produced by the backends.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetReading {
    /// Primary value: percent, level or count, depending on the widget.
    pub value: f32,
    /// Short caption under the value, e.g. "Charging".
    pub caption: String,
}

impl WidgetReading {
    pub fn percent(value: f32, caption: impl Into<String>) -> Self {
        Self {
            value: value.clamp(0.0, 100.0),
            caption: caption.into(),
        }
    }

    pub fn captioned(caption: impl Into<String>) -> Self {
        Self {
            value: 0.0,
            caption: caption.into(),
        }
    }
}

/// A widget size guard so the board math stays integer.
#[allow(dead_code)]
fn size_guard(size: Size) -> Size {
    size
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

    fn board() -> WidgetBoard {
        WidgetBoard::default()
    }

    #[test]
    fn default_board_flows_two_columns() {
        let board = board();
        assert!(!board.slots.is_empty());

        // No two same-row widgets overlap.
        for (i, slot) in board.slots.iter().enumerate() {
            for (_, other) in board.slots.iter().enumerate().skip(i + 1) {
                if slot.row == other.row {
                    let a = slot.rect(WidgetBoard::board_rect(WORK_AREA));
                    let b = other.rect(WidgetBoard::board_rect(WORK_AREA));
                    assert!(a.right() <= b.origin.x || b.right() <= a.origin.x);
                }
            }
        }
    }

    #[test]
    fn widgets_hit_by_position() {
        let board = board();
        let board_rect = WidgetBoard::board_rect(WORK_AREA);
        let first = board.slots[0].rect(board_rect);
        let centre = Point {
            x: first.origin.x + first.size.width / 2,
            y: first.origin.y + first.size.height / 2,
        };
        assert_eq!(board.widget_at(board_rect, centre), Some(0));

        let outside = Point { x: 100, y: 700 };
        assert_eq!(board.widget_at(board_rect, outside), None);
    }

    #[test]
    fn minus_buttons_only_hit_in_their_corner() {
        let board = board();
        let board_rect = WidgetBoard::board_rect(WORK_AREA);
        assert!(board.minus_hit(board_rect, 0, board.minus_center(board_rect, 0).unwrap()));
        let centre = Point {
            x: board_rect.origin.x + COLUMN_WIDTH / 2,
            y: board_rect.origin.y + 90,
        };
        assert!(!board.minus_hit(board_rect, 0, centre));
    }

    #[test]
    fn removing_a_widget_reflows_the_board() {
        let mut board = board();
        let before = board.slots.len();
        let removed = board.remove(0);
        assert_eq!(removed, Some(WidgetKind::Battery));
        assert_eq!(board.slots.len(), before - 1);
        // Slots are reflowed to dense positions.
        assert_eq!(board.slots[0].column, 0);
        assert_eq!(board.slots[0].row, 0);
    }

    #[test]
    fn popup_buttons_side_by_side() {
        let anchor = Rect::new(1000, 60, 180, 180);
        let popup = popup_rect(anchor, WORK_AREA);
        let (cancel, remove) = popup_buttons(popup);

        assert!(cancel.right() < remove.origin.x);
        assert_eq!(cancel.size.height, remove.size.height);

        let cancel_hit = Point {
            x: cancel.origin.x + 10,
            y: cancel.origin.y + 20,
        };
        assert!(matches!(popup_hit(popup, cancel_hit), PopupHit::Cancel));

        let remove_hit = Point {
            x: remove.origin.x + 10,
            y: remove.origin.y + 20,
        };
        assert!(matches!(popup_hit(popup, remove_hit), PopupHit::Remove));

        let outside = Point { x: 5, y: 5 };
        assert!(matches!(popup_hit(popup, outside), PopupHit::Outside));
    }

    #[test]
    fn wide_widgets_span_two_columns() {
        let board = WidgetBoard::from_kinds(vec![WidgetKind::Battery, WidgetKind::Clock]);
        assert!(board.slots[1].spans_two);
        let board_rect = WidgetBoard::board_rect(WORK_AREA);
        let clock = board.slots[1].rect(board_rect);
        assert_eq!(clock.size.width, 2 * COLUMN_WIDTH + GAP);
    }

    #[test]
    fn hold_threshold_is_reasonable() {
        assert!(HOLD_THRESHOLD >= Duration::from_millis(300));
        assert!(HOLD_THRESHOLD <= Duration::from_millis(600));
    }
}
