//! The macOS Control Centre: the dropdown panel from the top bar.
//!
//! The panel carries the classic grid: Wi-Fi, Bluetooth and Battery as
//! pill toggles on the left; a brightness slider beside a volume slider;
//! focus toggles below. Everything is laid out and hit-tested here.

use crate::windowing::{Point, Rect};

/// Width of the control centre panel.
pub const PANEL_WIDTH: i32 = 340;

/// Padding inside the panel.
pub const PANEL_PADDING: i32 = 12;

/// Size of one square toggle tile (Wi-Fi, Bluetooth, Focus...).
pub const TILE: i32 = 68;

/// Height of one slider row.
pub const SLIDER_HEIGHT: i32 = 40;

/// Corner radius of the glass panel.
pub const PANEL_RADIUS: i32 = 20;

/// Radius of the round toggle buttons inside tiles.
pub const TILE_BUTTON: i32 = 46;

/// One interactive control inside the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Wifi,
    Bluetooth,
    BatterySaver,
    Focus,
    DarkMode,
    /// The brightness slider; the point of interaction sets the value.
    Brightness,
    /// The volume slider.
    Volume,
}

/// The panel's full layout, resolved against a top-right origin.
#[derive(Debug, Clone, Copy)]
pub struct ControlLayout {
    /// The panel card.
    pub panel: Rect,
    /// Round tile buttons for Wi-Fi, Bluetooth and battery saver.
    pub wifi: Rect,
    pub bluetooth: Rect,
    pub battery_saver: Rect,
    /// The focus toggle beside the tiles.
    pub focus: Rect,
    pub dark_mode: Rect,
    /// The brightness slider row.
    pub brightness: Rect,
    /// The volume slider row.
    pub volume: Rect,
}

/// Where the panel drops from: the top bar's control-centre glyph.
pub fn panel_rect(work_area: Rect) -> Rect {
    Rect::new(
        work_area.right() - PANEL_WIDTH - 8,
        crate::topbar::BAR_HEIGHT + 6,
        PANEL_WIDTH,
        350,
    )
}

/// Lay the panel out below its anchor point.
pub fn layout(work_area: Rect) -> ControlLayout {
    let panel = panel_rect(work_area);

    let tiles_top = panel.origin.y + PANEL_PADDING;
    let wifi = Rect::new(panel.origin.x + PANEL_PADDING, tiles_top, TILE, TILE);
    let bluetooth = Rect::new(wifi.right() + 10, tiles_top, TILE, TILE);
    let battery_saver = Rect::new(bluetooth.right() + 10, tiles_top, TILE, TILE);

    // The focus pill spans the remaining width to the right of the tiles.
    let focus_left = battery_saver.right() + 10;
    let focus = Rect::new(
        focus_left,
        tiles_top,
        (panel.right() - PANEL_PADDING - focus_left).max(40),
        TILE,
    );

    let dark_mode = Rect::new(
        panel.origin.x + PANEL_PADDING,
        tiles_top + TILE + 10,
        panel.right() - PANEL_PADDING - panel.origin.x - PANEL_PADDING,
        36,
    );

    let brightness = Rect::new(
        panel.origin.x + PANEL_PADDING,
        dark_mode.bottom() + 12,
        panel.size.width - 2 * PANEL_PADDING,
        SLIDER_HEIGHT,
    );
    let volume = Rect::new(
        brightness.origin.x,
        brightness.bottom() + 10,
        brightness.size.width,
        SLIDER_HEIGHT,
    );

    ControlLayout {
        panel,
        wifi,
        bluetooth,
        battery_saver,
        focus,
        dark_mode,
        brightness,
        volume,
    }
}

/// Which control a point lands on, if any.
pub fn control_at(layout: &ControlLayout, point: Point) -> Option<Control> {
    if !layout.panel.contains_point(point) {
        return None;
    }
    if layout.wifi.contains_point(point) {
        Some(Control::Wifi)
    } else if layout.bluetooth.contains_point(point) {
        Some(Control::Bluetooth)
    } else if layout.battery_saver.contains_point(point) {
        Some(Control::BatterySaver)
    } else if layout.focus.contains_point(point) {
        Some(Control::Focus)
    } else if layout.dark_mode.contains_point(point) {
        Some(Control::DarkMode)
    } else if layout.brightness.contains_point(point) {
        Some(Control::Brightness)
    } else if layout.volume.contains_point(point) {
        Some(Control::Volume)
    } else {
        None
    }
}

/// Map a point inside a slider row to a 0.0..1.0 value.
pub fn slider_value(slider: Rect, point: Point) -> f32 {
    let inner = point.x - slider.origin.x - 8;
    let span = (slider.size.width - 16).max(1);
    (inner as f32 / span as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

    #[test]
    fn panel_hangs_from_the_top_right_below_the_bar() {
        let layout = layout(WORK_AREA);
        assert!(layout.panel.right() <= WORK_AREA.right() - 8);
        assert!(layout.panel.origin.y > crate::topbar::BAR_HEIGHT);
        assert_eq!(layout.panel.size.width, PANEL_WIDTH);
    }

    #[test]
    fn tiles_and_sliders_stay_inside_the_panel() {
        let layout = layout(WORK_AREA);
        for rect in [
            layout.wifi,
            layout.bluetooth,
            layout.battery_saver,
            layout.focus,
            layout.dark_mode,
            layout.brightness,
            layout.volume,
        ] {
            assert!(rect.origin.x >= layout.panel.origin.x);
            assert!(rect.right() <= layout.panel.right() + 1, "{rect:?}");
            assert!(rect.bottom() <= layout.panel.bottom() + 1, "{rect:?}");
        }
    }

    #[test]
    fn controls_resolve_by_position() {
        let layout = layout(WORK_AREA);

        let wifi_centre = Point {
            x: layout.wifi.origin.x + TILE / 2,
            y: layout.wifi.origin.y + TILE / 2,
        };
        assert_eq!(control_at(&layout, wifi_centre), Some(Control::Wifi));

        let volume_centre = Point {
            x: layout.volume.origin.x + 20,
            y: layout.volume.origin.y + SLIDER_HEIGHT / 2,
        };
        assert_eq!(control_at(&layout, volume_centre), Some(Control::Volume));

        let outside = Point { x: 10, y: 400 };
        assert_eq!(control_at(&layout, outside), None);
    }

    #[test]
    fn slider_values_map_across_the_track() {
        let slider = Rect::new(100, 50, 200, SLIDER_HEIGHT);
        let left = slider_value(slider, Point { x: 100, y: 70 });
        let right = slider_value(slider, Point { x: 300, y: 70 });
        let middle = slider_value(slider, Point { x: 200, y: 70 });

        assert!(left < 0.05);
        assert!(right > 0.95);
        assert!((middle - 0.5).abs() < 0.05);
    }
}
