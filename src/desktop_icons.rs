//! The desktop work area: a grid of icons like macOS and Windows.
//!
//! Icons are files, folders and shortcuts placed on the desktop. They snap
//! to a top-right grid (macOS alignment), support selection and activation,
//! and are positioned by the pure layout here.

use crate::desktop_entry::DesktopEntry;
use crate::windowing::{Point, Rect};

/// One desktop icon cell, in pixels.
pub const CELL: i32 = 84;

/// Label height under the icon.
pub const LABEL_HEIGHT: i32 = 22;

/// Grid gap.
pub const GAP: i32 = 12;

/// Top offset below the bar.
pub const TOP_MARGIN: i32 = 44;

/// Right margin beside the widget rail.
pub const RIGHT_MARGIN: i32 = 460;

/// What one desktop icon points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopTarget {
    /// A file or folder in the desktop directory.
    Path { path: String, directory: bool },
    /// A launcher shortcut to an application.
    Application { app_id: String },
}

/// One placed desktop icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopIcon {
    pub label: String,
    pub target: DesktopTarget,
    /// Zero-based slot index; position flows from it.
    pub slot: usize,
}

impl DesktopIcon {
    /// The icon's rect for its slot.
    pub fn rect(&self, work_area: Rect) -> Rect {
        let per_row = ((work_area.size.width - RIGHT_MARGIN) / (CELL + GAP)).max(1) as usize;
        let column = self.slot / per_row;
        let row = self.slot % per_row;
        Rect::new(
            work_area.right() - RIGHT_MARGIN - CELL - column as i32 * (CELL + GAP),
            work_area.origin.y + TOP_MARGIN + row as i32 * (CELL + LABEL_HEIGHT + GAP),
            CELL,
            CELL + LABEL_HEIGHT,
        )
    }
}

/// The desktop icon set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Desktop {
    pub icons: Vec<DesktopIcon>,
    /// The slot currently selected, if any.
    pub selected: Option<usize>,
}

impl Desktop {
    /// Map a directory listing into icon slots, alphabetical like macOS.
    pub fn from_listing(entries: &[(String, bool)], applications: &[DesktopEntry]) -> Self {
        let mut icons: Vec<DesktopIcon> = entries
            .iter()
            .map(|(label, directory)| DesktopIcon {
                label: label.clone(),
                target: DesktopTarget::Path {
                    path: label.clone(),
                    directory: *directory,
                },
                slot: 0,
            })
            .collect();

        // Shortcuts to applications land after the files.
        for app in applications {
            icons.push(DesktopIcon {
                label: app.name.clone(),
                target: DesktopTarget::Application {
                    app_id: app.id.clone(),
                },
                slot: 0,
            });
        }

        icons.sort_by_key(|icon| icon.label.to_lowercase());
        for (slot, icon) in icons.iter_mut().enumerate() {
            icon.slot = slot;
        }

        Self {
            icons,
            selected: None,
        }
    }

    /// The icon at a point, if any.
    pub fn icon_at(&self, work_area: Rect, point: Point) -> Option<usize> {
        self.icons
            .iter()
            .position(|icon| icon.rect(work_area).contains_point(point))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

    #[test]
    fn icons_flow_right_to_left_below_the_bar() {
        let desktop = Desktop::from_listing(&[("Report.pdf".into(), false), ("Projects".into(), true)], &[]);
        assert_eq!(desktop.icons.len(), 2);

        let first = desktop.icons[0].rect(WORK_AREA);
        assert!(first.origin.y >= TOP_MARGIN);
        assert!(first.right() <= WORK_AREA.right() - RIGHT_MARGIN + CELL + GAP);

        // Two icons in the same column never overlap.
        let second = desktop.icons[1].rect(WORK_AREA);
        assert!(second.bottom() <= first.origin.y || second.origin.y >= first.bottom());
    }

    #[test]
    fn many_icons_wrap_to_more_columns() {
        let listing: Vec<(String, bool)> = (0..30).map(|i| (format!("File{i}.txt"), false)).collect();
        let desktop = Desktop::from_listing(&listing, &[]);
        assert_eq!(desktop.icons.len(), 30);

        let rects: Vec<Rect> = desktop.icons.iter().map(|icon| icon.rect(WORK_AREA)).collect();
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
    fn icons_hit_by_position() {
        let desktop = Desktop::from_listing(&[("Notes.txt".into(), false)], &[]);
        let rect = desktop.icons[0].rect(WORK_AREA);
        let centre = Point {
            x: rect.origin.x + rect.size.width / 2,
            y: rect.origin.y + rect.size.height / 2,
        };
        assert_eq!(desktop.icon_at(WORK_AREA, centre), Some(0));

        let far = Point { x: 5, y: 500 };
        assert_eq!(desktop.icon_at(WORK_AREA, far), None);
    }

    #[test]
    fn listing_sorts_alphabetically_case_insensitive() {
        let desktop = Desktop::from_listing(&[("zeta".into(), false), ("Alpha".into(), true)], &[]);
        assert_eq!(desktop.icons[0].label, "Alpha");
        assert_eq!(desktop.icons[1].label, "zeta");
    }
}
