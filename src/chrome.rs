//! macOS-style window chrome: traffic-light buttons, title bar and hit-testing.
//!
//! Rouch draws its own chrome on top of client surfaces, exactly like macOS
//! compositors do for unadorned XDG toplevels. All geometry decisions are pure
//! and unit-tested, so the renderer and the input path can never disagree about
//! where a traffic light is.

use crate::windowing::{Point, Rect, ResizeEdge, Window};

/// Height of the chrome strip Rouch draws above client content.
pub const TITLE_BAR_HEIGHT: i32 = 36;

/// Diameter of one traffic-light circle, in logical pixels.
pub const TRAFFIC_LIGHT_DIAMETER: i32 = 12;

/// Space between the left window edge and the first traffic light.
pub const TRAFFIC_LIGHT_MARGIN: i32 = 13;

/// Space between the centers of two adjacent traffic lights.
pub const TRAFFIC_LIGHT_SPACING: i32 = 20;

/// The clickable slack around each circle, matching macOS generosity.
pub const TRAFFIC_LIGHT_HIT_SLOP: i32 = 2;

/// Visible thickness of the interactive resize border on the window edges.
pub const RESIZE_BORDER: i32 = 6;

/// Pre-multiplied RGBA of a traffic light, derived from the ocean palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficLightKind {
    Close,
    Minimize,
    Maximize,
}

impl TrafficLightKind {
    /// Ordered left to right, as on macOS.
    pub const ORDER: [Self; 3] = [Self::Close, Self::Minimize, Self::Maximize];

    /// Traffic-light colours tuned for the Liquid Material palette: warm red,
    /// amber and green over a translucent chrome background.
    pub const fn color(self) -> [f32; 4] {
        match self {
            Self::Close => [0.949, 0.186, 0.153, 1.0],
            Self::Minimize => [0.847, 0.627, 0.098, 1.0],
            Self::Maximize => [0.086, 0.647, 0.231, 1.0],
        }
    }

    /// Inactive window dimming, as macOS dims its lights when unfocused.
    pub const fn inactive_color(self) -> [f32; 4] {
        [0.42, 0.44, 0.46, 1.0]
    }

    /// Center of this light for a window whose frame is `frame`.
    pub fn center(self, frame: Rect) -> Point {
        let index = match self {
            Self::Close => 0,
            Self::Minimize => 1,
            Self::Maximize => 2,
        };
        Point {
            x: frame.origin.x
                + TRAFFIC_LIGHT_MARGIN
                + TRAFFIC_LIGHT_DIAMETER / 2
                + index * TRAFFIC_LIGHT_SPACING,
            y: frame.origin.y + TITLE_BAR_HEIGHT / 2,
        }
    }
}

/// One traffic light's interactive region, in global coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrafficLightHit {
    pub kind: TrafficLightKind,
    pub hit_rect: Rect,
}

impl TrafficLightHit {
    fn for_window(frame: Rect, kind: TrafficLightKind) -> Self {
        let center = kind.center(frame);
        let half = TRAFFIC_LIGHT_DIAMETER / 2 + TRAFFIC_LIGHT_HIT_SLOP;
        Self {
            kind,
            hit_rect: Rect::new(center.x - half, center.y - half, half * 2, half * 2),
        }
    }
}

/// Interactive regions of one window's chrome, in global coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chrome {
    frame: Rect,
    maximized: bool,
    fullscreen: bool,
}

impl Chrome {
    pub fn for_window(window: &Window) -> Self {
        Self {
            frame: window.frame(),
            maximized: window.maximized(),
            fullscreen: window.fullscreen(),
        }
    }

    pub fn frame(&self) -> Rect {
        self.frame
    }

    /// The strip the pointer can drag to move the window.
    pub fn title_bar(&self) -> Rect {
        if self.fullscreen {
            // No draggable chrome while a client owns the whole output.
            Rect::new(self.frame.origin.x, self.frame.origin.y, 0, 0)
        } else {
            Rect::new(
                self.frame.origin.x,
                self.frame.origin.y,
                self.frame.size.width,
                TITLE_BAR_HEIGHT,
            )
        }
    }
    pub fn traffic_lights(&self) -> [TrafficLightHit; 3] {
        let frame = self.frame;
        TrafficLightKind::ORDER.map(|kind| TrafficLightHit::for_window(frame, kind))
    }

    pub fn traffic_light_at(&self, point: Point) -> Option<TrafficLightKind> {
        if self.fullscreen {
            return None;
        }
        self.traffic_lights()
            .into_iter()
            .find(|light| contains(light.hit_rect, point))
            .map(|light| light.kind)
    }

    /// Chrome owns everything in the title bar that is not a traffic light;
    /// the client owns the rest of the window. Border hits are resolved after
    /// this, so overlapping regions prefer the resize cursor behaviour.
    pub fn hits_chrome(&self, point: Point) -> bool {
        if self.fullscreen {
            return false;
        }
        contains(self.title_bar(), point)
    }

    /// The resize border around a normal window. Fullscreen windows cannot be
    /// resized; maximized windows expose only the bottom edge like macOS.
    pub fn resize_edge_at(&self, point: Point) -> Option<ResizeEdge> {
        if self.fullscreen {
            return None;
        }

        let near_left = point.x < self.frame.origin.x + RESIZE_BORDER;
        let near_right = point.x >= self.frame.right() - RESIZE_BORDER;
        let near_top = point.y < self.frame.origin.y + RESIZE_BORDER;
        let near_bottom = point.y >= self.frame.bottom() - RESIZE_BORDER;

        let vertical = if near_top {
            Some(true)
        } else if near_bottom {
            Some(false)
        } else {
            None
        };
        let horizontal = if near_left {
            Some(true)
        } else if near_right {
            Some(false)
        } else {
            None
        };

        match (horizontal, vertical, self.maximized) {
            (None, None, _) => None,
            // A maximized window only reveals the bottom resize strip.
            (None, Some(false), true) => Some(ResizeEdge::Bottom),
            (Some(_), _, true) | (_, Some(true), true) => None,
            (Some(true), Some(true), false) => Some(ResizeEdge::TopLeft),
            (Some(true), Some(false), false) => Some(ResizeEdge::BottomLeft),
            (Some(false), Some(true), false) => Some(ResizeEdge::TopRight),
            (Some(false), Some(false), false) => Some(ResizeEdge::BottomRight),
            (Some(true), None, false) => Some(ResizeEdge::Left),
            (Some(false), None, false) => Some(ResizeEdge::Right),
            (None, Some(true), false) => Some(ResizeEdge::Top),
            (None, Some(false), false) => Some(ResizeEdge::Bottom),
        }
    }

    /// The area where client content is allowed to paint. Rouch reserves the
    /// title bar for its own chrome, so client geometry is shifted down.
    pub fn client_area(&self) -> Rect {
        if self.fullscreen {
            self.frame
        } else {
            Rect::new(
                self.frame.origin.x,
                self.frame.origin.y + TITLE_BAR_HEIGHT,
                self.frame.size.width,
                (self.frame.size.height - TITLE_BAR_HEIGHT).max(0),
            )
        }
    }
}

fn contains(rect: Rect, point: Point) -> bool {
    point.x >= rect.origin.x && point.x < rect.right() && point.y >= rect.origin.y && point.y < rect.bottom()
}

/// Where a pointer press inside a window's bounds should go.
///
/// Chrome actions win over client content, because the chrome is drawn on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeAction {
    /// The press belongs to the client surface.
    Client,
    /// Drag the window by its title bar.
    DragTitleBar,
    /// Start an interactive resize from this edge.
    Resize(ResizeEdge),
    /// A traffic-light button was pressed.
    TrafficLight(TrafficLightKind),
}

/// Resolve one pointer press against a window's chrome.
pub fn action_at(window: &Window, point: Point) -> ChromeAction {
    let chrome = Chrome::for_window(window);

    if let Some(kind) = chrome.traffic_light_at(point) {
        return ChromeAction::TrafficLight(kind);
    }
    if let Some(edge) = chrome.resize_edge_at(point) {
        return ChromeAction::Resize(edge);
    }
    if chrome.hits_chrome(point) {
        return ChromeAction::DragTitleBar;
    }
    ChromeAction::Client
}

/// Title text Rouch renders in the center of the title bar. A `None` title
/// renders no text, exactly like macOS does for ownerless titles.
pub fn title_text(title: Option<&str>) -> Option<&str> {
    title.filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windowing::{Size, SizeLimits, WindowManager};

    fn window_at(x: i32, y: i32) -> (WindowManager, crate::windowing::WindowId) {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());
        let _ = manager.move_by(id, Point { x: x - 96, y: y - 72 });
        (manager, id)
    }

    #[test]
    fn traffic_lights_are_inside_the_title_bar() {
        let (manager, id) = window_at(96, 72);
        let window = manager.window(id).unwrap();
        let chrome = Chrome::for_window(window);
        let bar = chrome.title_bar();

        for light in chrome.traffic_lights() {
            let center = light.kind.center(window.frame());
            assert!(contains(bar, center));
            assert!(chrome.traffic_light_at(center) == Some(light.kind));
        }
    }

    #[test]
    fn traffic_light_hitboxes_are_generous_but_ordered() {
        let (manager, id) = window_at(96, 72);
        let window = manager.window(id).unwrap();
        let chrome = Chrome::for_window(window);

        let close_center = TrafficLightKind::Close.center(window.frame());
        let min_center = TrafficLightKind::Minimize.center(window.frame());
        assert!(close_center.x < min_center.x);

        // A point two pixels outside the close circle still hits it.
        let outside = Point {
            x: close_center.x - TRAFFIC_LIGHT_DIAMETER / 2 - 1,
            y: close_center.y,
        };
        assert_eq!(chrome.traffic_light_at(outside), Some(TrafficLightKind::Close));

        let far = Point {
            x: close_center.x + 60,
            y: close_center.y,
        };
        assert_eq!(chrome.traffic_light_at(far), None);
    }

    #[test]
    fn client_area_excludes_the_title_bar() {
        let (manager, id) = window_at(96, 72);
        let window = manager.window(id).unwrap();
        let chrome = Chrome::for_window(window);

        let client = chrome.client_area();
        assert_eq!(client.origin.y, window.frame().origin.y + TITLE_BAR_HEIGHT);
        assert_eq!(client.size.height, window.frame().size.height - TITLE_BAR_HEIGHT);
        assert_eq!(client.size.width, window.frame().size.width);
    }

    #[test]
    fn chrome_takes_precedence_over_client_in_the_title_bar() {
        let (manager, id) = window_at(96, 72);
        let window = manager.window(id).unwrap();

        let title_bar_middle = Point {
            x: window.frame().origin.x + window.frame().size.width / 2,
            y: window.frame().origin.y + TITLE_BAR_HEIGHT / 2,
        };
        assert_eq!(action_at(window, title_bar_middle), ChromeAction::DragTitleBar);

        let content = Point {
            x: window.frame().origin.x + 100,
            y: window.frame().origin.y + TITLE_BAR_HEIGHT + 50,
        };
        assert_eq!(action_at(window, content), ChromeAction::Client);
    }

    #[test]
    fn edges_resolve_to_resize_actions() {
        let (manager, id) = window_at(96, 72);
        let window = manager.window(id).unwrap();
        let frame = window.frame();

        let right_edge = Point {
            x: frame.right() - 1,
            y: frame.origin.y + frame.size.height / 2,
        };
        assert_eq!(
            action_at(window, right_edge),
            ChromeAction::Resize(ResizeEdge::Right)
        );

        let bottom_right_corner = Point {
            x: frame.right() - 1,
            y: frame.bottom() - 1,
        };
        assert_eq!(
            action_at(window, bottom_right_corner),
            ChromeAction::Resize(ResizeEdge::BottomRight)
        );
    }

    #[test]
    fn fullscreen_hides_all_chrome_interactions() {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());
        assert!(manager.enter_fullscreen(id));
        let window = manager.window(id).unwrap();

        let frame = window.frame();
        let any_point = Point {
            x: frame.origin.x + 20,
            y: frame.origin.y + 10,
        };
        assert_eq!(action_at(window, any_point), ChromeAction::Client);

        let center = Point {
            x: frame.origin.x + frame.size.width / 2,
            y: frame.origin.y + frame.size.height / 2,
        };
        assert_eq!(action_at(window, center), ChromeAction::Client);

        let chrome = Chrome::for_window(window);
        assert_eq!(chrome.title_bar().size, Size::new(0, 0));
        assert_eq!(chrome.client_area(), frame);
    }

    #[test]
    fn maximized_windows_only_resize_from_the_bottom() {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());
        assert!(manager.maximize(id));
        let window = manager.window(id).unwrap();
        let frame = window.frame();

        let bottom = Point {
            x: frame.origin.x + frame.size.width / 2,
            y: frame.bottom() - 1,
        };
        assert_eq!(
            action_at(window, bottom),
            ChromeAction::Resize(ResizeEdge::Bottom)
        );

        let top = Point {
            x: frame.origin.x + frame.size.width / 2,
            y: frame.origin.y + 1,
        };
        assert_eq!(action_at(window, top), ChromeAction::DragTitleBar);

        let right = Point {
            x: frame.right() - 1,
            y: frame.origin.y + frame.size.height / 2,
        };
        // No resize handle on the sides of a maximized window; the client
        // keeps receiving presses there, exactly like a plain content click.
        assert_eq!(action_at(window, right), ChromeAction::Client);
    }
}
