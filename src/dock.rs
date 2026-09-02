//! The macOS dock behaviour, expressed as pure geometry.
//!
//! Every visual rule of the Cupertino dock lives here: the glass panel, the
//! gaussian magnification that grows neighbouring icons as the pointer
//! approaches, the running-indicator dots, the bounce a launching icon plays,
//! and the minimized-window shelf. The renderer only samples these functions;
//! it never invents layout on its own.

use crate::windowing::{Point, Rect, Size};

/// Base edge length of a dock icon, in logical pixels.
pub const ICON_SIZE: i32 = 56;

/// Edge length of the largest magnified icon, matching macOS behaviour.
pub const MAX_ICON_SIZE: i32 = 88;

/// Horizontal gap between two icons at rest.
pub const ICON_SPACING: i32 = 16;

/// Extra magnified gap, inserted proportionally as icons grow.
pub const MAX_SPACING: i32 = 26;

/// Padding inside the panel around the icon row.
pub const PANEL_PADDING: i32 = 10;

/// Height of the rounded glass panel at rest.
pub const PANEL_HEIGHT: i32 = ICON_SIZE + 2 * PANEL_PADDING;

/// Radius of the panel's corners; macOS uses a soft squircle-ish round.
pub const PANEL_RADIUS: i32 = 24;

/// Radius of the running-application indicator dot.
pub const DOT_RADIUS: i32 = 3;

/// Space between an icon's bottom edge and its indicator dot.
pub const DOT_OFFSET: i32 = 4;

/// Bounce apex of a launching icon, in logical pixels.
pub const BOUNCE_HEIGHT: i32 = 42;

/// Where the dock sits. macOS keeps a small breathing gap from the screen edge.
pub const EDGE_MARGIN: i32 = 8;

/// Vertical margin of the tooltip label above the magnified icon.
pub const TOOLTIP_OFFSET: i32 = 14;

/// One slot in the dock: a pinned launcher or a live window group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockItem {
    /// Identifier of the application this slot belongs to.
    pub app_id: String,
    /// A short label for the tooltip, normally the pretty application name.
    pub label: String,
    /// True when at least one window of this application is running.
    pub running: bool,
    /// True while the application is launching and the icon should bounce.
    pub launching: bool,
    /// Windows of this application that are minimized. macOS keeps them on
    /// the dock right side; Rouch shows them as badges on their own icon.
    pub minimized_windows: usize,
}

impl DockItem {
    pub fn pinned(app_id: &str, label: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            label: label.to_owned(),
            running: false,
            launching: false,
            minimized_windows: 0,
        }
    }
}

/// The dock state the compositor owns, mirrored into this pure model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockModel {
    items: Vec<DockItem>,
}

impl Default for DockModel {
    fn default() -> Self {
        Self::new()
    }
}

impl DockModel {
    /// A dock with the default Rouch pinned set.
    pub fn new() -> Self {
        Self {
            items: default_pins(),
        }
    }

    pub fn from_items(items: Vec<DockItem>) -> Self {
        Self { items }
    }

    pub fn items(&self) -> &[DockItem] {
        &self.items
    }

    pub fn item(&self, app_id: &str) -> Option<&DockItem> {
        self.items.iter().find(|item| item.app_id == app_id)
    }

    /// Register a window mapping under its application. Unknown applications
    /// are appended to the dock exactly like macOS does on first launch.
    pub fn window_mapped(&mut self, app_id: &str, label: &str) {
        if let Some(item) = self.items.iter_mut().find(|item| item.app_id == app_id) {
            item.running = true;
            item.launching = false;
        } else {
            self.items.push(DockItem {
                app_id: app_id.to_owned(),
                label: label.to_owned(),
                running: true,
                launching: false,
                minimized_windows: 0,
            });
        }
    }

    /// Drop the running state when an application's last window is gone.
    pub fn window_unmapped(&mut self, app_id: &str, still_minimized: usize) {
        if let Some(item) = self.items.iter_mut().find(|item| item.app_id == app_id) {
            item.running = still_minimized > 0;
            item.minimized_windows = still_minimized;
        }
    }

    /// Track a minimize or restore for the indicator badge.
    pub fn set_minimized(&mut self, app_id: &str, count: usize) {
        if let Some(item) = self.items.iter_mut().find(|item| item.app_id == app_id) {
            item.minimized_windows = count;
        }
    }

    /// Mark an application as launching; its icon bounces until mapped.
    pub fn start_launch(&mut self, app_id: &str, label: &str) {
        if let Some(item) = self.items.iter_mut().find(|item| item.app_id == app_id) {
            item.launching = true;
        } else {
            self.items.push(DockItem {
                app_id: app_id.to_owned(),
                label: label.to_owned(),
                running: false,
                launching: true,
                minimized_windows: 0,
            });
        }
    }

    /// The full panel rectangle, centred above the bottom edge of the work
    /// area, with the pointer-driven magnification already applied.
    pub fn panel(&self, work_area: Rect, pointer: Option<Point>) -> Rect {
        let layout = self.layout(work_area, pointer);
        layout.panel
    }

    /// The panel at rest, used for measuring when no pointer is nearby.
    pub fn resting_panel(&self, work_area: Rect) -> Rect {
        self.panel(work_area, None)
    }

    /// Lay the icon row out in one pass.
    ///
    /// At rest the icons are evenly spaced. With a pointer, each icon grows
    /// by a bell curve of its distance to the pointer, and the row is
    /// anchored so the icon under the pointer keeps its centre there while
    /// neighbours are pushed aside — exactly the macOS feel.
    fn layout(&self, work_area: Rect, pointer: Option<Point>) -> Layout {
        let count = self.items.len().max(1);
        let resting_row = count as i32 * ICON_SIZE + (count as i32 - 1) * ICON_SPACING;
        let panel_bottom = work_area.bottom() - EDGE_MARGIN;
        let icons_bottom = panel_bottom - PANEL_PADDING;

        match pointer {
            None => {
                let start_x = work_area.origin.x + (work_area.size.width - resting_row) / 2;
                let icons: Vec<Rect> = (0..count)
                    .map(|index| {
                        let left = start_x + index as i32 * (ICON_SIZE + ICON_SPACING);
                        Rect::new(left, icons_bottom - ICON_SIZE, ICON_SIZE, ICON_SIZE)
                    })
                    .collect();
                let panel = Rect::new(
                    start_x - PANEL_PADDING,
                    panel_bottom - PANEL_HEIGHT,
                    resting_row + 2 * PANEL_PADDING,
                    PANEL_HEIGHT,
                );
                Layout { panel, icons }
            }
            Some(pointer) => {
                // Resting centres drive the bell; find the hovered icon.
                let resting_start = work_area.origin.x + (work_area.size.width - resting_row) / 2;
                let stride = (ICON_SIZE + ICON_SPACING) as f32;
                let centres: Vec<f32> = (0..count)
                    .map(|index| {
                        (resting_start + index as i32 * (ICON_SIZE + ICON_SPACING) + ICON_SIZE / 2) as f32
                    })
                    .collect();

                let hovered = centres
                    .iter()
                    .enumerate()
                    .min_by(|a, b| {
                        (a.1 - pointer.x as f32)
                            .abs()
                            .partial_cmp(&(b.1 - pointer.x as f32).abs())
                            .expect("distances are finite")
                    })
                    .map(|(index, _)| index)
                    .unwrap_or(0);

                // Bell magnification from distance in strides.
                let sizes: Vec<f32> = centres
                    .iter()
                    .map(|centre| {
                        let distance = ((pointer.x as f32 - centre) / stride).abs();
                        let scale = bell(distance);
                        ICON_SIZE as f32 + (MAX_ICON_SIZE - ICON_SIZE) as f32 * scale
                    })
                    .collect();

                // Walk outward from the hovered icon, widening gaps as the
                // two adjacent icons grow.
                let mut lefts = vec![0.0f32; count];
                lefts[hovered] = pointer.x as f32 - sizes[hovered] / 2.0;
                for index in (0..hovered).rev() {
                    let gap = gap_between(&centres, &sizes, index, pointer.x as f32);
                    lefts[index] = lefts[index + 1] - gap - sizes[index];
                }
                for index in hovered + 1..count {
                    let gap = gap_between(&centres, &sizes, index - 1, pointer.x as f32);
                    lefts[index] = lefts[index - 1] + sizes[index - 1] + gap;
                }

                let icons: Vec<Rect> = (0..count)
                    .map(|index| {
                        let size = sizes[index].round() as i32;
                        let left = lefts[index].round() as i32;
                        Rect::new(left, icons_bottom - size, size, size)
                    })
                    .collect();

                let min_left = icons.iter().map(|icon| icon.origin.x).min().unwrap_or(0);
                let max_right = icons.iter().map(|icon| icon.right()).max().unwrap_or(0);
                let panel = Rect::new(
                    min_left - PANEL_PADDING,
                    panel_bottom - (MAX_ICON_SIZE + 2 * PANEL_PADDING),
                    max_right - min_left + 2 * PANEL_PADDING,
                    MAX_ICON_SIZE + 2 * PANEL_PADDING,
                );
                Layout { panel, icons }
            }
        }
    }

    /// The geometric description of one icon, including magnification and
    /// launch bounce.
    pub fn icon_geometry(
        &self,
        index: usize,
        work_area: Rect,
        pointer: Option<Point>,
        bounce: f32,
    ) -> IconGeometry {
        let layout = self.layout(work_area, pointer);
        let rect = layout
            .icons
            .get(index)
            .copied()
            .unwrap_or_else(|| Rect::new(0, 0, ICON_SIZE, ICON_SIZE));

        let lift = (bounce * BOUNCE_HEIGHT as f32).round() as i32;
        IconGeometry {
            rect: Rect::new(
                rect.origin.x,
                rect.origin.y - lift,
                rect.size.width,
                rect.size.height,
            ),
            scale: rect.size.width as f32 / ICON_SIZE as f32,
        }
    }

    /// Which dock slot a pointer press claims, if any.
    pub fn hit(&self, work_area: Rect, pointer: Point) -> Option<usize> {
        let layout = self.layout(work_area, Some(pointer));
        if !layout.panel.contains_point(pointer) {
            return None;
        }
        layout.icons.iter().position(|icon| icon.contains_point(pointer))
    }

    /// Centre of the icon's indicator dot.
    pub fn dot_center(&self, index: usize, work_area: Rect, pointer: Option<Point>) -> Point {
        let layout = self.layout(work_area, pointer);
        let icon = layout
            .icons
            .get(index)
            .copied()
            .unwrap_or_else(|| Rect::new(0, 0, ICON_SIZE, ICON_SIZE));
        Point {
            x: icon.origin.x + icon.size.width / 2,
            y: icon.bottom() + DOT_OFFSET + DOT_RADIUS,
        }
    }
}

/// One laid-out frame of the dock.
struct Layout {
    panel: Rect,
    icons: Vec<Rect>,
}

/// The drawn geometry of one icon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconGeometry {
    /// The icon's square frame after magnification and bounce.
    pub rect: Rect,
    /// Scale relative to [`ICON_SIZE`]; the renderer bakes icons at 1.0.
    pub scale: f32,
}

/// The magnification bell: full growth at distance zero, falling to nothing
/// at [`MAG_REACH`] strides, with a soft quadratic profile.
fn bell(distance_strides: f32) -> f32 {
    const REACH: f32 = 1.6;
    if distance_strides >= REACH {
        return 0.0;
    }
    let base = 1.0 - distance_strides / REACH;
    base * base
}

/// The gap between two icons, widened proportionally to how much the pair
/// is magnified.
fn gap_between(centres: &[f32], sizes: &[f32], left_index: usize, pointer: f32) -> f32 {
    let stride = (ICON_SIZE + ICON_SPACING) as f32;
    let scale = |index: usize| {
        let distance = ((pointer - centres[index]) / stride).abs();
        bell(distance)
    };
    let pair = (scale(left_index) + scale(left_index + 1)) / 2.0;
    let _ = sizes;
    ICON_SPACING as f32 + (MAX_SPACING - ICON_SPACING) as f32 * pair
}

/// Rouch's default pinned set. App identifiers follow desktop-entry names.
fn default_pins() -> Vec<DockItem> {
    vec![
        DockItem::pinned("launcher", "Launcher"),
        DockItem::pinned("app-store", "App Gallery"),
        DockItem::pinned("org.gnome.Files", "Finder"),
        DockItem::pinned("rouch-terminal", "Terminal"),
        DockItem::pinned("org.mozilla.firefox", "Firefox"),
        DockItem::pinned("org.gnome.TextEditor", "Text Editor"),
        DockItem::pinned("settings", "Settings"),
    ]
}

/// Cover-fit geometry: the source crop rectangle that fills `target` without
/// distortion, used by the wallpaper and by icon badges.
pub fn cover_fit(source: Size, target: Size) -> Rect {
    if source.width <= 0 || source.height <= 0 || target.width <= 0 || target.height <= 0 {
        return Rect::new(0, 0, 0, 0);
    }

    let source_ratio = source.width as f32 / source.height as f32;
    let target_ratio = target.width as f32 / target.height as f32;

    if source_ratio > target_ratio {
        // Source is wider: crop the sides.
        let crop_width = (target.width as f32 * source.height as f32 / target.height as f32).round() as i32;
        let x = (source.width - crop_width) / 2;
        Rect::new(x, 0, crop_width, source.height)
    } else {
        // Source is taller: crop the top and bottom.
        let crop_height = (target.height as f32 * source.width as f32 / target.width as f32).round() as i32;
        let y = (source.height - crop_height) / 2;
        Rect::new(0, y, source.width, crop_height)
    }
}

/// The one-shot easing a launching icon's bounce follows: a smooth arch that
/// lifts the icon, holds briefly and settles. `phase` is 0.0..1.0 over the
/// bounce cycle.
pub fn bounce_offset(phase: f32) -> f32 {
    let t = phase.clamp(0.0, 1.0);
    // One full sine arch, sharp enough to clear the panel edge on launch.
    (t * std::f32::consts::PI).sin()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

    #[test]
    fn panel_is_centred_near_the_bottom() {
        let dock = DockModel::new();
        let panel = dock.resting_panel(WORK_AREA);

        let expected_width = dock.items().len() as i32 * ICON_SIZE
            + (dock.items().len() as i32 - 1) * ICON_SPACING
            + 2 * PANEL_PADDING;
        assert_eq!(panel.size.width, expected_width);
        assert_eq!(panel.size.height, PANEL_HEIGHT);
        assert_eq!(panel.bottom(), WORK_AREA.bottom() - EDGE_MARGIN);

        let centre = panel.origin.x + panel.size.width / 2;
        assert_eq!(centre, WORK_AREA.size.width / 2);
    }

    #[test]
    fn magnification_grows_the_hovered_icon_most() {
        let dock = DockModel::new();
        let count = dock.items().len();
        let pointer = dock.icon_geometry(2, WORK_AREA, None, 0.0).rect.origin;
        let pointer = Point {
            x: pointer.x + ICON_SIZE / 2,
            y: pointer.y + ICON_SIZE / 2,
        };

        let hovered = dock.icon_geometry(2, WORK_AREA, Some(pointer), 0.0);
        let neighbour = dock.icon_geometry(3, WORK_AREA, Some(pointer), 0.0);
        let far = dock.icon_geometry(count - 1, WORK_AREA, Some(pointer), 0.0);

        assert!(hovered.rect.size.width > ICON_SIZE);
        assert!(hovered.rect.size.width > neighbour.rect.size.width);
        assert!(neighbour.rect.size.width >= far.rect.size.width);
        assert!(far.rect.size.width <= ICON_SIZE + 2);
    }

    #[test]
    fn icons_never_overlap_at_rest() {
        let dock = DockModel::new();
        let mut previous_right = i32::MIN;
        for index in 0..dock.items().len() {
            let icon = dock.icon_geometry(index, WORK_AREA, None, 0.0);
            assert!(icon.rect.right() > previous_right);
            previous_right = icon.rect.right();
        }
    }

    #[test]
    fn bounce_lifts_the_icon_and_settles() {
        assert!(bounce_offset(0.25) > 0.5);
        assert!(bounce_offset(0.0).abs() < 1e-4);
        assert!(bounce_offset(1.0).abs() < 1e-3);
    }

    #[test]
    fn running_apps_gain_and_lose_dots() {
        let mut dock = DockModel::new();
        dock.window_mapped("rouch-terminal", "Terminal");
        assert!(dock.item("rouch-terminal").unwrap().running);
        assert!(!dock.item("rouch-terminal").unwrap().launching);

        dock.start_launch("org.unknown.App", "Unknown");
        assert!(dock.item("org.unknown.App").unwrap().launching);

        dock.window_mapped("org.unknown.App", "Unknown");
        assert!(!dock.item("org.unknown.App").unwrap().launching);

        dock.window_unmapped("rouch-terminal", 0);
        assert!(!dock.item("rouch-terminal").unwrap().running);
    }

    #[test]
    fn unknown_apps_are_appended_to_the_right() {
        let mut dock = DockModel::new();
        let before = dock.items().len();
        dock.window_mapped("com.example.New", "New");
        assert_eq!(dock.items().len(), before + 1);
        assert_eq!(dock.items().last().unwrap().app_id, "com.example.New");
    }

    #[test]
    fn minimized_windows_show_on_their_app_icon() {
        let mut dock = DockModel::new();
        dock.window_mapped("rouch-terminal", "Terminal");
        dock.set_minimized("rouch-terminal", 2);
        assert_eq!(dock.item("rouch-terminal").unwrap().minimized_windows, 2);
        assert!(dock.item("rouch-terminal").unwrap().running);
    }

    #[test]
    fn pointer_press_inside_an_icon_hits_it() {
        let dock = DockModel::new();
        let icon = dock.icon_geometry(1, WORK_AREA, None, 0.0);
        let centre = Point {
            x: icon.rect.origin.x + icon.rect.size.width / 2,
            y: icon.rect.origin.y + icon.rect.size.height / 2,
        };
        assert_eq!(dock.hit(WORK_AREA, centre), Some(1));

        let outside = Point { x: 20, y: 20 };
        assert_eq!(dock.hit(WORK_AREA, outside), None);
    }

    #[test]
    fn cover_fit_crops_without_distortion() {
        let crop = cover_fit(Size::new(1920, 1080), Size::new(1440, 900));
        assert_eq!(crop.size.height, 1080);
        let ratio = crop.size.width as f32 / crop.size.height as f32;
        let target = 1440.0 / 900.0;
        assert!((ratio - target).abs() < 0.01);

        let crop = cover_fit(Size::new(600, 1200), Size::new(1440, 900));
        assert_eq!(crop.size.width, 600);
    }
}
