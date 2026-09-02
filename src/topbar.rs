//! The macOS-style top bar and its menus, as pure geometry and state.
//!
//! The bar itself is a translucent strip carrying, left to right: the
//! distribution mark (where Cupertino puts an apple), the active
//! application's name, then the status cluster â€” control centre, Wi-Fi,
//!! battery percentage, clock and date â€” ending at the right edge.

use crate::windowing::{Point, Rect};

/// Height of the top bar.
pub const BAR_HEIGHT: i32 = 28;

/// Padding inside the bar around status items.
pub const ITEM_PADDING: i32 = 10;

/// Width the distribution mark's slot occupies on the left.
pub const MARK_SLOT: i32 = 44;

/// The top bar's rectangle over the work area.
pub fn bar_rect(work_area: Rect) -> Rect {
    Rect::new(
        work_area.origin.x,
        work_area.origin.y,
        work_area.size.width,
        BAR_HEIGHT,
    )
}

/// Which part of the bar a point lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopBarItem {
    /// The distribution mark, standing in for the Apple menu.
    DistributionMark,
    /// The active application name.
    AppName,
    /// The control-centre toggle.
    ControlCentre,
    /// The clock, which opens notification centre on click.
    Clock,
    /// The battery indicator.
    Battery,
    /// None of the interactive items.
    Background,
}

/// Fixed layout of one bar, resolved against a work area and the text of
/// the clock and active application.
#[derive(Debug, Clone, Copy)]
pub struct TopBarLayout {
    /// The whole bar strip.
    pub bar: Rect,
    /// The distribution mark button.
    pub mark: Rect,
    /// The active application's name region.
    pub app_name: Rect,
    /// The control-centre button.
    pub control_centre: Rect,
    /// The battery indicator.
    pub battery: Rect,
    /// The clock button.
    pub clock: Rect,
}

/// Lay the top bar out. The clock text width is provided by the caller
/// because the renderer owns the font measurement.
pub fn layout(work_area: Rect, app_name_width: i32, clock_width: i32) -> TopBarLayout {
    let bar = bar_rect(work_area);
    let centre_y = bar.origin.y + BAR_HEIGHT / 2;

    let _ = centre_y;
    let mark = Rect::new(bar.origin.x + 4, bar.origin.y, MARK_SLOT, BAR_HEIGHT);
    let app_name = Rect::new(
        mark.right() + 2,
        bar.origin.y,
        app_name_width + 2 * ITEM_PADDING,
        BAR_HEIGHT,
    );

    // The status cluster is right-aligned: clock at the edge, then battery,
    // then the control-centre glyph.
    let clock = Rect::new(
        bar.right() - clock_width - 2 * ITEM_PADDING,
        bar.origin.y,
        clock_width + 2 * ITEM_PADDING,
        BAR_HEIGHT,
    );
    let battery = Rect::new(clock.origin.x - 64, bar.origin.y, 64, BAR_HEIGHT);
    let control_centre = Rect::new(battery.origin.x - 40, bar.origin.y, 40, BAR_HEIGHT);

    TopBarLayout {
        bar,
        mark,
        app_name,
        control_centre,
        battery,
        clock,
    }
}

/// Resolve a press inside the bar to its item.
pub fn item_at(layout: &TopBarLayout, point: Point) -> TopBarItem {
    if layout.mark.contains_point(point) {
        TopBarItem::DistributionMark
    } else if layout.app_name.contains_point(point) {
        TopBarItem::AppName
    } else if layout.control_centre.contains_point(point) {
        TopBarItem::ControlCentre
    } else if layout.battery.contains_point(point) {
        TopBarItem::Battery
    } else if layout.clock.contains_point(point) {
        TopBarItem::Clock
    } else {
        TopBarItem::Background
    }
}

/// The linux distribution Rouch is running on, for the bar's left mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Distribution {
    #[default]
    Unknown,
    Arch,
    Debian,
    Fedora,
    Ubuntu,
    Gentoo,
    NixOS,
    Mint,
    Manjaro,
    /// Styled after the openSUSE project's own capitalisation.
    #[allow(non_camel_case_types)]
    openSuse,
}

impl Distribution {
    /// Detect the host distribution from `/etc/os-release`.
    pub fn detect(os_release: &str) -> Self {
        // The ID field, e.g. `ID=arch`.
        let id = os_release
            .lines()
            .find_map(|line| line.strip_prefix("ID="))
            .map(|id| id.trim().trim_matches('"').to_ascii_lowercase())
            .unwrap_or_default();

        match id.as_str() {
            "arch" | "archarm" => Self::Arch,
            "debian" => Self::Debian,
            "fedora" => Self::Fedora,
            "ubuntu" => Self::Ubuntu,
            "gentoo" => Self::Gentoo,
            "nixos" => Self::NixOS,
            "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" => Self::openSuse,
            "linuxmint" => Self::Mint,
            "manjaro" => Self::Manjaro,
            _ => Self::Unknown,
        }
    }

    /// The short name shown in menus, lowercase like the Apple menu's "About".
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Linux",
            Self::Arch => "Arch",
            Self::Debian => "Debian",
            Self::Fedora => "Fedora",
            Self::Ubuntu => "Ubuntu",
            Self::Gentoo => "Gentoo",
            Self::NixOS => "NixOS",
            Self::openSuse => "openSUSE",
            Self::Mint => "Mint",
            Self::Manjaro => "Manjaro",
        }
    }

    /// The badge letter the bar mark draws inside the penguin silhouette.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Unknown => "L",
            Self::Arch => "A",
            Self::Debian => "D",
            Self::Fedora => "F",
            Self::Ubuntu => "U",
            Self::Gentoo => "G",
            Self::NixOS => "N",
            Self::openSuse => "S",
            Self::Mint => "M",
            Self::Manjaro => "J",
        }
    }
}

/// The clock text, macOS style: "Tue 30 Aug 14:05".
pub fn clock_text(now_secs: u64) -> String {
    // A tiny civil-time formatter: no datetime crate, just epoch arithmetic.
    let days_total = now_secs / 86_400;
    let rem = now_secs % 86_400;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;

    // Weekday: epoch (1970-01-01) was a Thursday.
    let weekday = match (days_total + 3) % 7 {
        0 => "Mon",
        1 => "Tue",
        2 => "Wed",
        3 => "Thu",
        4 => "Fri",
        5 => "Sat",
        _ => "Sun",
    };
    // Day of month: walk the Gregorian calendar from the epoch.
    let (month, day) = civil_from_days(days_total as i64);

    format!("{weekday} {day} {month} {hour:02}:{minute:02}")
}

/// Civil month and day from days since the epoch (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (&'static str, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let _ = y;
    let month = match m {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    };
    (month, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

    #[test]
    fn bar_spans_the_full_width_at_the_top() {
        let layout = layout(WORK_AREA, 60, 90);
        assert_eq!(layout.bar, bar_rect(WORK_AREA));
        assert_eq!(layout.bar.size.height, BAR_HEIGHT);
        assert_eq!(layout.bar.origin.y, 0);
    }

    #[test]
    fn items_are_left_to_right_and_never_overlap() {
        let layout = layout(WORK_AREA, 60, 90);
        assert!(layout.mark.right() <= layout.app_name.origin.x);
        assert!(layout.app_name.right() < layout.control_centre.origin.x);
        assert!(layout.control_centre.right() <= layout.battery.origin.x);
        assert!(layout.battery.right() <= layout.clock.origin.x);
        assert_eq!(layout.clock.right(), WORK_AREA.size.width);
    }

    #[test]
    fn presses_resolve_to_bar_items() {
        let layout = layout(WORK_AREA, 60, 90);

        let mark = Point { x: 8, y: 10 };
        assert_eq!(item_at(&layout, mark), TopBarItem::DistributionMark);

        let cc = Point {
            x: layout.control_centre.origin.x + 5,
            y: 10,
        };
        assert_eq!(item_at(&layout, cc), TopBarItem::ControlCentre);

        let clock = Point {
            x: layout.clock.origin.x + 5,
            y: 10,
        };
        assert_eq!(item_at(&layout, clock), TopBarItem::Clock);

        let none = Point { x: 700, y: 10 };
        assert_eq!(item_at(&layout, none), TopBarItem::Background);
    }

    #[test]
    fn distribution_detects_os_release_ids() {
        let arch = "NAME=\"Arch Linux\"\nID=arch\n";
        assert_eq!(Distribution::detect(arch), Distribution::Arch);
        assert_eq!(Distribution::detect("ID=ubuntu\n"), Distribution::Ubuntu);
        assert_eq!(Distribution::detect("ID=\"fedora\"\n"), Distribution::Fedora);
        assert_eq!(Distribution::detect("nope"), Distribution::Unknown);
    }

    #[test]
    fn clock_formats_macos_style() {
        // "Www D Mon HH:MM" â€” four space-separated tokens.
        let secs: u64 = 1_800_000_000;
        let text = clock_text(secs);
        let parts: Vec<&str> = text.split(' ').collect();
        assert_eq!(parts.len(), 4, "got {text}");
        assert!(parts[3].contains(':'));
        assert!(text.contains("Jan"));

        // Epoch formats to Thursday, 1 Jan, midnight.
        assert_eq!(clock_text(0), "Thu 1 Jan 00:00");
    }
}
