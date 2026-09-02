//! The macOS Tahoe application launcher: the menu that opens from the mark.
//!
//! It is a centred glass sheet with a search field on top and a grid of
//! application tiles below, filtered live as the user types. Navigation is
//! pure: the search text maps to a filtered view of the app database.

/// Columns of the app grid.
pub const GRID_COLUMNS: usize = 7;

/// One application tile's size.
pub const TILE_SIZE: i32 = 92;

/// Gap between tiles.
pub const GAP: i32 = 18;

/// Height of the search field band.
pub const SEARCH_HEIGHT: i32 = 44;

/// The launcher sheet's rectangle, centred on the work area.
pub fn sheet_rect(work_area: crate::windowing::Rect) -> crate::windowing::Rect {
    crate::windowing::Rect::new(
        work_area.origin.x + (work_area.size.width - 760).max(0) / 2,
        work_area.origin.y + 120,
        760.min(work_area.size.width - 32),
        460.min(work_area.size.height - 160),
    )
}

/// Filter the application list by the current search text.
///
/// Matching is case-insensitive on the application name; an empty query
/// shows everything. A query with no hits yields an empty view — the
/// renderer then draws the macOS-style "No Results" line.
pub fn filter_apps<'a>(
    apps: &'a [super::desktop_entry::DesktopEntry],
    query: &str,
) -> Vec<&'a super::desktop_entry::DesktopEntry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return apps.iter().collect();
    }
    apps.iter()
        .filter(|app| app.name.to_lowercase().contains(&query))
        .collect()
}

/// The grid position of one filtered index.
pub fn grid_slot(index: usize) -> (usize, usize) {
    (index % GRID_COLUMNS, index / GRID_COLUMNS)
}

/// The tile rect of one filtered index inside the sheet.
pub fn tile_rect(sheet: crate::windowing::Rect, index: usize) -> crate::windowing::Rect {
    let (column, row) = grid_slot(index);
    crate::windowing::Rect::new(
        sheet.origin.x + 28 + column as i32 * (TILE_SIZE + GAP),
        sheet.origin.y + SEARCH_HEIGHT + 18 + row as i32 * (TILE_SIZE + GAP),
        TILE_SIZE,
        TILE_SIZE,
    )
}

/// The search field rect inside the sheet.
pub fn search_rect(sheet: crate::windowing::Rect) -> crate::windowing::Rect {
    crate::windowing::Rect::new(
        sheet.origin.x + (sheet.size.width - 320) / 2,
        sheet.origin.y + 10,
        320,
        30,
    )
}

/// Which tile a point hits, if any, for the filtered set.
pub fn tile_at(sheet: crate::windowing::Rect, count: usize, point: crate::windowing::Point) -> Option<usize> {
    (0..count)
        .map(|index| tile_rect(sheet, index))
        .position(|rect| rect.contains_point(point))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_entry::DesktopEntry;
    use crate::windowing::{Point, Rect};

    fn app(name: &str) -> DesktopEntry {
        DesktopEntry {
            id: name.to_lowercase(),
            name: name.to_owned(),
            icon: String::new(),
            exec: String::new(),
            categories: vec![],
            no_display: false,
        }
    }

    const SHEET: Rect = Rect::new(340, 120, 760, 460);

    #[test]
    fn empty_query_shows_everything() {
        let apps = vec![app("Files"), app("Terminal"), app("Firefox")];
        assert_eq!(filter_apps(&apps, "").len(), 3);
        assert_eq!(filter_apps(&apps, "  ").len(), 3);
    }

    #[test]
    fn search_is_substring_case_insensitive() {
        let apps = vec![app("Firefox"), app("Files"), app("Terminal")];
        assert_eq!(filter_apps(&apps, "fi").len(), 2);
        assert_eq!(filter_apps(&apps, "TERM").len(), 1);
        assert_eq!(filter_apps(&apps, "zzz").len(), 0);
    }

    #[test]
    fn grid_wraps_at_seven_columns() {
        assert_eq!(grid_slot(0), (0, 0));
        assert_eq!(grid_slot(6), (6, 0));
        assert_eq!(grid_slot(7), (0, 1));
        assert_eq!(grid_slot(15), (1, 2));
    }

    #[test]
    fn tiles_hit_by_position() {
        let second = tile_rect(SHEET, 1);
        let centre = Point {
            x: second.origin.x + TILE_SIZE / 2,
            y: second.origin.y + TILE_SIZE / 2,
        };
        assert_eq!(tile_at(SHEET, 10, centre), Some(1));

        let outside = Point { x: 2, y: 2 };
        assert_eq!(tile_at(SHEET, 10, outside), None);
    }

    #[test]
    fn search_field_sits_at_the_top_centre() {
        let search = search_rect(SHEET);
        assert!(search.origin.y < SHEET.origin.y + SEARCH_HEIGHT);
        let centre = search.origin.x + search.size.width / 2;
        assert_eq!(centre, SHEET.origin.x + SHEET.size.width / 2);
    }
}
