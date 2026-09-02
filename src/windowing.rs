//! Desktop-window semantics independent from Wayland transport or rendering.
//!
//! This module is the compositor's small, deterministic window-management
//! core. It owns focus and MRU state, workspaces, z-layers, geometry and
//! restore state, but deliberately knows nothing about Wayland objects or a
//! renderer. A backend can therefore apply the pure results to whichever
//! protocol or output it owns.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(u64);

impl WindowId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

impl Size {
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            origin: Point { x, y },
            size: Size { width, height },
        }
    }

    pub const fn right(self) -> i32 {
        self.origin.x.saturating_add(self.size.width)
    }

    pub const fn bottom(self) -> i32 {
        self.origin.y.saturating_add(self.size.height)
    }

    pub const fn contains_point(self, point: Point) -> bool {
        point.x >= self.origin.x
            && point.x < self.right()
            && point.y >= self.origin.y
            && point.y < self.bottom()
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.origin.x < other.right()
            && other.origin.x < self.right()
            && self.origin.y < other.bottom()
            && other.origin.y < self.bottom()
    }
}

/// How much of a dragged window must remain reachable inside the work area.
/// macOS keeps a strip of the title bar on screen so a window is never lost.
pub const RETENTION: i32 = 96;

/// The virtual work area used before an output backend reports a physical
/// monitor. A future output update replaces this value without changing the
/// rest of the window-management rules.
pub const DEFAULT_WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeLimits {
    pub minimum: Size,
    pub maximum: Option<Size>,
}

impl Default for SizeLimits {
    fn default() -> Self {
        Self {
            minimum: Size::new(160, 120),
            maximum: None,
        }
    }
}

impl SizeLimits {
    /// Constrain a client request without allowing malformed negative or zero
    /// dimensions to enter the geometry state. If a client reports a maximum
    /// below its minimum, the minimum wins; this keeps the invariant useful
    /// instead of producing a frame that violates both bounds.
    pub fn constrain(self, requested: Size) -> Size {
        let minimum = Size::new(self.minimum.width.max(1), self.minimum.height.max(1));
        let mut width = requested.width.max(minimum.width);
        let mut height = requested.height.max(minimum.height);

        if let Some(maximum) = self.maximum {
            width = width.min(maximum.width.max(minimum.width));
            height = height.min(maximum.height.max(minimum.height));
        }

        Size::new(width, height)
    }
}

/// A stable, one-based virtual desktop identifier. Workspace `1` always
/// exists; IDs above it are managed by [`WindowManager::create_workspace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(u32);

impl WorkspaceId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl Default for WorkspaceId {
    fn default() -> Self {
        Self(1)
    }
}

/// Z-order layers are sorted from back to front. A pinned window is sticky
/// across workspaces; [`AlwaysOnTop`](Self::AlwaysOnTop) controls its layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum WindowLayer {
    Background,
    #[default]
    Normal,
    AlwaysOnTop,
    Overlay,
}

/// Standard macOS-like edge and corner destinations for a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapPosition {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl SnapPosition {
    /// Resolve a snap destination against a work area. The manager supplies a
    /// positive work area, while this method remains useful to integrations
    /// that want to preview a destination without mutating any state.
    pub fn rect(self, work_area: Rect) -> Rect {
        let width = work_area.size.width.max(1);
        let height = work_area.size.height.max(1);
        let split_x = (width / 2).max(1).min(width);
        let split_y = (height / 2).max(1).min(height);
        let right_width = (width - split_x).max(1);
        let bottom_height = (height - split_y).max(1);
        let right_origin = work_area.origin.x.saturating_add(split_x);
        let bottom_origin = work_area.origin.y.saturating_add(split_y);

        match self {
            Self::Left => Rect::new(work_area.origin.x, work_area.origin.y, split_x, height),
            Self::Right => Rect::new(right_origin, work_area.origin.y, right_width, height),
            Self::Top => Rect::new(work_area.origin.x, work_area.origin.y, width, split_y),
            Self::Bottom => Rect::new(work_area.origin.x, bottom_origin, width, bottom_height),
            Self::TopLeft => Rect::new(work_area.origin.x, work_area.origin.y, split_x, split_y),
            Self::TopRight => Rect::new(right_origin, work_area.origin.y, right_width, split_y),
            Self::BottomLeft => Rect::new(work_area.origin.x, bottom_origin, split_x, bottom_height),
            Self::BottomRight => Rect::new(right_origin, bottom_origin, right_width, bottom_height),
        }
    }
}

/// Deterministic layouts for assigning all ordinary windows in the current
/// workspace to non-overlapping regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileLayout {
    Columns,
    Rows,
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl ResizeEdge {
    const fn moves_left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    const fn moves_right(self) -> bool {
        matches!(self, Self::Right | Self::TopRight | Self::BottomRight)
    }

    const fn moves_top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }

    const fn moves_bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::BottomLeft | Self::BottomRight)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FullscreenRestore {
    frame: Rect,
    maximized: bool,
    snap: Option<SnapPosition>,
    tiled: bool,
}

/// All state the renderer needs to decide whether and where to draw a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    id: WindowId,
    frame: Rect,
    normal_frame: Rect,
    fullscreen_restore: Option<FullscreenRestore>,
    limits: SizeLimits,
    active: bool,
    minimized: bool,
    maximized: bool,
    fullscreen: bool,
    close_requested: bool,
    title: String,
    workspace: WorkspaceId,
    layer: WindowLayer,
    pinned: bool,
    snap: Option<SnapPosition>,
    tiled: bool,
}

impl Window {
    pub const fn id(&self) -> WindowId {
        self.id
    }

    pub const fn frame(&self) -> Rect {
        self.frame
    }

    pub const fn normal_frame(&self) -> Rect {
        self.normal_frame
    }

    pub const fn active(&self) -> bool {
        self.active
    }

    pub const fn minimized(&self) -> bool {
        self.minimized
    }

    pub const fn maximized(&self) -> bool {
        self.maximized
    }

    pub const fn fullscreen(&self) -> bool {
        self.fullscreen
    }

    pub const fn close_requested(&self) -> bool {
        self.close_requested
    }

    pub const fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    pub const fn layer(&self) -> WindowLayer {
        self.layer
    }

    /// A pinned window is sticky and is visible on every workspace unless it
    /// is minimized.
    pub const fn pinned(&self) -> bool {
        self.pinned
    }

    pub const fn always_on_top(&self) -> bool {
        matches!(self.layer, WindowLayer::AlwaysOnTop | WindowLayer::Overlay)
    }

    pub const fn snap_position(&self) -> Option<SnapPosition> {
        self.snap
    }

    pub const fn snapped(&self) -> bool {
        self.snap.is_some()
    }

    pub const fn tiled(&self) -> bool {
        self.tiled
    }

    pub fn visible_on(&self, workspace: WorkspaceId) -> bool {
        !self.minimized && (self.pinned || self.workspace == workspace)
    }

    /// The client-supplied title, if any. The chrome renders it centered.
    pub fn title(&self) -> &str {
        &self.title
    }
}

#[derive(Debug, Clone)]
struct TabCycle {
    origin: WindowId,
    candidates: Vec<WindowId>,
    index: usize,
}

/// Windows are stored from back to front. Layer ordering is authoritative;
/// focus raises a window within its layer, while a pinned always-on-top window
/// can intentionally remain above the active normal window.
#[derive(Debug)]
pub struct WindowManager {
    work_area: Rect,
    next_id: u64,
    windows: Vec<Window>,
    mru: Vec<WindowId>,
    workspace_count: u32,
    current_workspace: WorkspaceId,
    overview: bool,
    tab_cycle: Option<TabCycle>,
    tile_layout: Option<TileLayout>,
}

impl Default for WindowManager {
    fn default() -> Self {
        Self::new(DEFAULT_WORK_AREA)
    }
}

impl WindowManager {
    pub const fn new(work_area: Rect) -> Self {
        Self {
            work_area,
            next_id: 1,
            windows: Vec::new(),
            mru: Vec::new(),
            workspace_count: 1,
            current_workspace: WorkspaceId(1),
            overview: false,
            tab_cycle: None,
            tile_layout: None,
        }
    }

    pub const fn work_area(&self) -> Rect {
        self.work_area
    }

    /// All windows in back-to-front render order, including other workspaces.
    /// Use [`Self::visible_windows`] when consuming only the current desktop.
    pub fn windows(&self) -> &[Window] {
        &self.windows
    }

    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|window| window.id == id)
    }

    pub fn active_window(&self) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|window| window.active && self.is_visible(window))
            .map(Window::id)
    }

    /// Alias useful to input integrations that call the state “focused”.
    pub fn focused_window(&self) -> Option<WindowId> {
        self.active_window()
    }

    /// The first item is the most recently focused window. The list contains
    /// minimized and off-workspace windows so it can also back a dock/history
    /// view; Alt-Tab filters it to focusable windows.
    pub fn mru(&self) -> &[WindowId] {
        &self.mru
    }

    pub fn mru_order(&self) -> &[WindowId] {
        self.mru()
    }

    pub fn focus_order(&self) -> &[WindowId] {
        self.mru()
    }

    /// Focusable windows in most-recently-used order. Shell integrations use
    /// this instead of reconstructing MRU from the render stack, so Alt-Tab
    /// remains correct when layers or workspaces reorder that stack.
    pub fn focusable_window_ids(&self) -> Vec<WindowId> {
        self.focusable_mru_ids()
    }

    /// Current-workspace windows in back-to-front order. Pinned windows are
    /// included, while minimized and other-workspace windows are omitted.
    pub fn visible_windows(&self) -> Vec<&Window> {
        self.windows
            .iter()
            .filter(|window| self.is_visible(window))
            .collect()
    }

    pub fn visible_window_ids(&self) -> Vec<WindowId> {
        self.visible_windows().into_iter().map(Window::id).collect()
    }

    pub fn workspaces(&self) -> Vec<WorkspaceId> {
        (1..=self.workspace_count).map(WorkspaceId).collect()
    }

    pub const fn workspace_count(&self) -> u32 {
        self.workspace_count
    }

    pub const fn current_workspace(&self) -> WorkspaceId {
        self.current_workspace
    }

    pub const fn overview_active(&self) -> bool {
        self.overview
    }

    pub const fn tab_cycle_active(&self) -> bool {
        self.tab_cycle.is_some()
    }

    pub fn workspace_exists(&self, workspace: WorkspaceId) -> bool {
        workspace.raw() >= 1 && workspace.raw() <= self.workspace_count
    }

    pub fn create_window(&mut self, requested_size: Size, limits: SizeLimits) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);

        let size = limits.constrain(requested_size);
        let cascade = ((id.raw().saturating_sub(1) % 8) as i32).saturating_mul(28);
        let frame = self.clamp_frame(Rect::new(
            96_i32.saturating_add(cascade),
            72_i32.saturating_add(cascade),
            size.width,
            size.height,
        ));
        let window = Window {
            id,
            frame,
            normal_frame: frame,
            fullscreen_restore: None,
            limits,
            active: false,
            minimized: false,
            maximized: false,
            fullscreen: false,
            close_requested: false,
            title: String::new(),
            workspace: self.current_workspace,
            layer: WindowLayer::Normal,
            pinned: false,
            snap: None,
            tiled: false,
        };

        self.windows.push(window);
        self.normalize_stack();
        let _ = self.focus(id);
        id
    }

    /// Move to an existing virtual desktop. Deleting a desktop is the only
    /// operation that renumbers IDs; all other workspace IDs are stable.
    pub fn switch_workspace(&mut self, workspace: WorkspaceId) -> bool {
        if !self.workspace_exists(workspace) {
            return false;
        }

        self.current_workspace = workspace;
        self.overview = false;
        self.tab_cycle = None;
        self.ensure_focus_for_workspace();
        true
    }

    pub fn set_current_workspace(&mut self, workspace: WorkspaceId) -> bool {
        self.switch_workspace(workspace)
    }

    /// Relative workspace navigation, wrapping around at either end.
    pub fn switch_workspace_by(&mut self, delta: i32) -> bool {
        let count = i64::from(self.workspace_count);
        let current = i64::from(self.current_workspace.raw().saturating_sub(1));
        let next = (current + i64::from(delta)).rem_euclid(count);
        self.switch_workspace(WorkspaceId((next as u32).saturating_add(1)))
    }

    pub fn create_workspace(&mut self) -> WorkspaceId {
        if self.workspace_count == u32::MAX {
            return WorkspaceId(self.workspace_count);
        }
        self.workspace_count += 1;
        WorkspaceId(self.workspace_count)
    }

    /// Remove a desktop and move its windows onto the next desktop (or the
    /// previous one when removing the last). IDs above the removed desktop
    /// shift down by one, like a compact desktop list in a shell overview.
    pub fn remove_workspace(&mut self, workspace: WorkspaceId) -> bool {
        let raw = workspace.raw();
        if self.workspace_count <= 1 || !self.workspace_exists(workspace) {
            return false;
        }

        let old_count = self.workspace_count;
        let replacement = if raw == old_count { raw - 1 } else { raw };
        for window in &mut self.windows {
            let window_workspace = window.workspace.raw();
            let mapped = if window_workspace == raw {
                replacement
            } else if window_workspace > raw {
                window_workspace - 1
            } else {
                window_workspace
            };
            window.workspace = WorkspaceId(mapped);
        }

        let current = self.current_workspace.raw();
        self.workspace_count -= 1;
        self.current_workspace = WorkspaceId(if current == raw {
            replacement
        } else if current > raw {
            current - 1
        } else {
            current
        });
        self.tile_layout = None;
        self.tab_cycle = None;
        self.ensure_focus_for_workspace();
        true
    }

    pub fn move_to_workspace(&mut self, id: WindowId, workspace: WorkspaceId) -> bool {
        if !self.workspace_exists(workspace) {
            return false;
        }
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        if self.windows[index].workspace == workspace {
            return true;
        }

        let was_active = self.windows[index].active;
        self.windows[index].workspace = workspace;
        if was_active && !self.is_visible(&self.windows[index]) {
            self.windows[index].active = false;
            self.focus_topmost_visible();
        } else if workspace == self.current_workspace && !self.windows[index].minimized {
            let _ = self.focus(id);
        }
        true
    }

    pub fn send_to_workspace(&mut self, id: WindowId, workspace: WorkspaceId) -> bool {
        self.move_to_workspace(id, workspace)
    }

    /// Update the output/work area and repair every normal frame. This is
    /// intentionally also applied to minimized and hidden windows: restoring
    /// either must never resurrect an unreachable frame.
    pub fn set_work_area(&mut self, work_area: Rect) {
        self.work_area = work_area;
        let area = self.effective_work_area();
        for window in &mut self.windows {
            window.normal_frame = clamp_frame_to_area(area, window.normal_frame);
            if window.maximized || window.fullscreen {
                window.frame = area;
            } else if let Some(snap) = window.snap {
                window.frame = fit_rect_to_region(area, snap.rect(area), window.limits);
            } else {
                window.frame = clamp_frame_to_area(area, window.frame);
            }
        }

        if let Some(layout) = self.tile_layout {
            let _ = self.tile_workspace(layout);
        }
    }

    /// Focus also raises a window within its layer. A minimized or
    /// off-workspace window cannot keep focus, which avoids invisible keyboard
    /// focus. A close request does not hide a client: XDG lets an application
    /// decline it, so it remains interactive until it destroys its toplevel.
    pub fn focus(&mut self, id: WindowId) -> bool {
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        if !self.is_visible(&self.windows[index]) {
            return false;
        }

        for window in &mut self.windows {
            window.active = false;
        }
        let mut window = self.windows.remove(index);
        window.active = true;
        self.windows.push(window);
        self.normalize_stack();
        self.touch_mru(id);
        true
    }

    pub fn clear_focus(&mut self) {
        for window in &mut self.windows {
            window.active = false;
        }
    }

    /// Sets the client-supplied title, for the chrome and any future dock.
    pub fn set_title(&mut self, id: WindowId, title: &str) -> bool {
        let Some(window) = self.window_mut(id) else {
            return false;
        };
        if window.title == title {
            return false;
        }
        window.title = title.to_owned();
        true
    }

    pub fn set_layer(&mut self, id: WindowId, layer: WindowLayer) -> bool {
        let Some(window) = self.window_mut(id) else {
            return false;
        };
        if window.layer == layer {
            return false;
        }
        window.layer = layer;
        self.normalize_stack();
        true
    }

    pub fn set_always_on_top(&mut self, id: WindowId, always_on_top: bool) -> bool {
        let layer = if always_on_top {
            WindowLayer::AlwaysOnTop
        } else {
            WindowLayer::Normal
        };
        self.set_layer(id, layer)
    }

    pub fn toggle_always_on_top(&mut self, id: WindowId) -> bool {
        let Some(window) = self.window(id) else {
            return false;
        };
        self.set_always_on_top(id, !window.always_on_top())
    }

    /// Pinning makes a window sticky across all workspaces without changing
    /// its z-layer. Turning it off while focused immediately hands focus to a
    /// valid window on the current workspace if necessary.
    pub fn set_pinned(&mut self, id: WindowId, pinned: bool) -> bool {
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        if self.windows[index].pinned == pinned {
            return false;
        }
        self.windows[index].pinned = pinned;
        if !pinned && !self.is_visible(&self.windows[index]) && self.windows[index].active {
            self.windows[index].active = false;
            self.focus_topmost_visible();
        }
        true
    }

    pub fn toggle_pinned(&mut self, id: WindowId) -> bool {
        let Some(window) = self.window(id) else {
            return false;
        };
        self.set_pinned(id, !window.pinned())
    }

    /// Move a window to an absolute position. Snapped/tiled windows restore
    /// their normal frame first, matching the usual drag-to-unsnap gesture.
    /// At least [`RETENTION`] pixels remain reachable on each axis.
    pub fn move_to(&mut self, id: WindowId, position: Point) -> Option<Rect> {
        let area = self.effective_work_area();
        let index = self.windows.iter().position(|window| window.id == id)?;
        let window = &mut self.windows[index];
        if window.minimized || window.maximized || window.fullscreen {
            return None;
        }
        if window.snap.is_some() || window.tiled {
            window.frame = window.normal_frame;
            window.snap = None;
            window.tiled = false;
            self.tile_layout = None;
        }

        let size = window.frame.size;
        let origin = clamp_origin_to_area(area, size, position);
        window.frame = Rect { origin, size };
        window.normal_frame = window.frame;
        Some(window.frame)
    }

    pub fn move_by(&mut self, id: WindowId, delta: Point) -> Option<Rect> {
        let window = self.window(id)?;
        if window.minimized || window.maximized || window.fullscreen {
            return None;
        }
        let origin = if window.snap.is_some() || window.tiled {
            window.normal_frame.origin
        } else {
            window.frame.origin
        };
        let position = Point::new(origin.x.saturating_add(delta.x), origin.y.saturating_add(delta.y));
        self.move_to(id, position)
    }

    pub fn resize_by(&mut self, id: WindowId, edge: ResizeEdge, delta: Point) -> Option<Rect> {
        let area = self.effective_work_area();
        let index = self.windows.iter().position(|window| window.id == id)?;
        let window = &mut self.windows[index];
        if window.minimized || window.maximized || window.fullscreen {
            return None;
        }
        if window.snap.is_some() || window.tiled {
            window.frame = window.normal_frame;
            window.snap = None;
            window.tiled = false;
            self.tile_layout = None;
        }

        let old = window.frame;
        let requested = Size::new(
            if edge.moves_left() {
                old.size.width.saturating_sub(delta.x)
            } else if edge.moves_right() {
                old.size.width.saturating_add(delta.x)
            } else {
                old.size.width
            },
            if edge.moves_top() {
                old.size.height.saturating_sub(delta.y)
            } else if edge.moves_bottom() {
                old.size.height.saturating_add(delta.y)
            } else {
                old.size.height
            },
        );
        let size = window.limits.constrain(requested);
        let origin = Point::new(
            if edge.moves_left() {
                old.right().saturating_sub(size.width)
            } else {
                old.origin.x
            },
            if edge.moves_top() {
                old.bottom().saturating_sub(size.height)
            } else {
                old.origin.y
            },
        );
        let frame = clamp_frame_to_area(area, Rect { origin, size });
        window.frame = frame;
        window.normal_frame = frame;
        Some(frame)
    }

    pub fn minimize(&mut self, id: WindowId) -> bool {
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        let was_active = self.windows[index].active;
        self.windows[index].minimized = true;
        self.windows[index].active = false;
        self.tab_cycle = None;

        if was_active || self.active_window().is_none() {
            self.focus_topmost_visible();
        }
        true
    }

    pub fn restore(&mut self, id: WindowId) -> bool {
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        self.windows[index].minimized = false;
        if self.is_visible(&self.windows[index]) {
            let _ = self.focus(id);
        } else if self.active_window().is_none() {
            self.focus_topmost_visible();
        }
        true
    }

    pub fn toggle_minimized(&mut self, id: WindowId) -> bool {
        let Some(window) = self.window(id) else {
            return false;
        };
        if window.minimized() {
            self.restore(id)
        } else {
            self.minimize(id)
        }
    }

    /// Idempotent form used for `xdg_toplevel.set_maximized`.
    pub fn maximize(&mut self, id: WindowId) -> bool {
        if self.window(id).is_none() {
            return false;
        }
        if self.window(id).is_some_and(Window::fullscreen) {
            let _ = self.exit_fullscreen(id);
        }

        let area = self.effective_work_area();
        let current_workspace = self.current_workspace;
        let index = self.windows.iter().position(|window| window.id == id).unwrap();
        let window = &mut self.windows[index];
        window.minimized = false;
        if !window.maximized {
            if window.snap.is_none() && !window.tiled {
                window.normal_frame = window.frame;
            }
            window.maximized = true;
            window.frame = area;
            window.snap = None;
            window.tiled = false;
            self.tile_layout = None;
        }
        if window.visible_on(current_workspace) {
            self.focus(id)
        } else {
            // A protocol request is still valid for a window on another
            // workspace; it simply must not steal the current workspace's
            // focus.
            true
        }
    }

    pub fn toggle_maximized(&mut self, id: WindowId) -> bool {
        let Some(window) = self.window(id) else {
            return false;
        };
        if window.maximized {
            self.unmaximize(id)
        } else {
            self.maximize(id)
        }
    }

    pub fn unmaximize(&mut self, id: WindowId) -> bool {
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        if !self.windows[index].maximized {
            return false;
        }

        let normal = self.windows[index].normal_frame;
        self.windows[index].maximized = false;
        self.windows[index].snap = None;
        self.windows[index].tiled = false;
        self.windows[index].frame = self.clamp_frame(normal);
        self.tile_layout = None;
        if self.windows[index].visible_on(self.current_workspace) {
            self.focus(id)
        } else {
            true
        }
    }

    pub fn enter_fullscreen(&mut self, id: WindowId) -> bool {
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        if self.windows[index].minimized {
            self.windows[index].minimized = false;
        }
        if self.windows[index].fullscreen {
            return self.focus(id);
        }

        let restore = FullscreenRestore {
            frame: self.windows[index].frame,
            maximized: self.windows[index].maximized,
            snap: self.windows[index].snap,
            tiled: self.windows[index].tiled,
        };
        let area = self.effective_work_area();
        let current_workspace = self.current_workspace;
        let window = &mut self.windows[index];
        window.fullscreen_restore = Some(restore);
        window.fullscreen = true;
        window.maximized = false;
        window.snap = None;
        window.tiled = false;
        window.frame = area;
        self.tile_layout = None;
        if window.visible_on(current_workspace) {
            self.focus(id)
        } else {
            true
        }
    }

    pub fn exit_fullscreen(&mut self, id: WindowId) -> bool {
        let area = self.effective_work_area();
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        if !self.windows[index].fullscreen {
            return false;
        }

        let restore = self.windows[index]
            .fullscreen_restore
            .take()
            .unwrap_or(FullscreenRestore {
                frame: self.windows[index].normal_frame,
                maximized: false,
                snap: None,
                tiled: false,
            });
        let window = &mut self.windows[index];
        window.frame = if restore.maximized {
            area
        } else if let Some(snap) = restore.snap {
            fit_rect_to_region(area, snap.rect(area), window.limits)
        } else {
            clamp_frame_to_area(area, restore.frame)
        };
        window.maximized = restore.maximized;
        window.snap = restore.snap;
        window.tiled = restore.tiled;
        window.fullscreen = false;
        if window.visible_on(self.current_workspace) {
            self.focus(id)
        } else {
            true
        }
    }

    /// Flips fullscreen state like the macOS green traffic light and
    /// Super+F both do. Returns whether anything changed.
    pub fn toggle_fullscreen(&mut self, id: WindowId) -> bool {
        match self.window(id) {
            Some(window) if window.fullscreen() => self.exit_fullscreen(id),
            Some(_) => self.enter_fullscreen(id),
            None => false,
        }
    }

    /// Assign a standard snap region and focus the result. A normal frame is
    /// saved exactly once so unsnap, maximize and fullscreen can restore it.
    pub fn snap_window(&mut self, id: WindowId, position: SnapPosition) -> Option<Rect> {
        self.snap_to_rect(id, position.rect(self.effective_work_area()), Some(position))
    }

    pub fn snap(&mut self, id: WindowId, position: SnapPosition) -> Option<Rect> {
        self.snap_window(id, position)
    }

    /// Apply an arbitrary pure tiling region. The optional standard position
    /// is kept only when the caller used one of the named snap destinations.
    pub fn snap_to_rect(
        &mut self,
        id: WindowId,
        region: Rect,
        position: Option<SnapPosition>,
    ) -> Option<Rect> {
        self.window(id)?;
        if self.window(id).is_some_and(Window::fullscreen) {
            let _ = self.exit_fullscreen(id);
        }

        let area = self.effective_work_area();
        let index = self.windows.iter().position(|window| window.id == id).unwrap();
        let window = &mut self.windows[index];
        if window.maximized {
            window.maximized = false;
            window.frame = window.normal_frame;
        } else if !window.tiled && window.snap.is_none() {
            window.normal_frame = window.frame;
        }
        window.minimized = false;
        window.frame = fit_rect_to_region(area, region, window.limits);
        window.maximized = false;
        window.snap = position;
        window.tiled = true;
        self.tile_layout = None;
        let frame = window.frame;
        let _ = self.focus(id);
        Some(frame)
    }

    pub fn unsnap(&mut self, id: WindowId) -> bool {
        self.untile(id)
    }

    /// Restore the normal frame of a snapped or tiled window.
    pub fn untile(&mut self, id: WindowId) -> bool {
        let area = self.effective_work_area();
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        if !self.windows[index].tiled && self.windows[index].snap.is_none() {
            return false;
        }
        let normal = self.windows[index].normal_frame;
        self.windows[index].frame = clamp_frame_to_area(area, normal);
        self.windows[index].snap = None;
        self.windows[index].tiled = false;
        self.tile_layout = None;
        true
    }

    pub fn tile_window(&mut self, id: WindowId, position: SnapPosition) -> Option<Rect> {
        self.snap_window(id, position)
    }

    /// Tile ordinary, non-pinned windows in the current workspace. Pinned
    /// utility windows remain independent so a workspace layout cannot move a
    /// sticky panel differently on each desktop.
    pub fn tile_workspace(&mut self, layout: TileLayout) -> Vec<(WindowId, Rect)> {
        let area = self.effective_work_area();
        let ids = self
            .focusable_mru_ids()
            .into_iter()
            .filter(|id| {
                self.window(*id).is_some_and(|window| {
                    window.workspace == self.current_workspace
                        && !window.pinned
                        && !window.maximized
                        && !window.fullscreen
                })
            })
            .collect::<Vec<_>>();
        if ids.is_empty() {
            self.tile_layout = None;
            return Vec::new();
        }

        let regions = tile_regions(area, ids.len(), layout);
        let mut result = Vec::with_capacity(ids.len());
        for (id, region) in ids.into_iter().zip(regions) {
            let index = self.windows.iter().position(|window| window.id == id).unwrap();
            let window = &mut self.windows[index];
            if !window.tiled {
                window.normal_frame = window.frame;
            }
            window.frame = fit_rect_to_region(area, region, window.limits);
            window.snap = None;
            window.tiled = true;
            result.push((id, window.frame));
        }
        self.tile_layout = Some(layout);
        result
    }

    pub fn untile_workspace(&mut self) -> usize {
        let ids = self
            .windows
            .iter()
            .filter(|window| window.workspace == self.current_workspace && !window.pinned && window.tiled)
            .map(Window::id)
            .collect::<Vec<_>>();
        let mut restored = 0;
        for id in ids {
            if self.untile(id) {
                restored += 1;
            }
        }
        self.tile_layout = None;
        restored
    }

    /// Return the next/previous focus target according to MRU order. Passing
    /// `true` models Alt-Tab; passing `false` models Alt-Shift-Tab.
    pub fn alt_tab(&mut self, forward: bool) -> Option<WindowId> {
        let candidates = self.focusable_mru_ids();
        if candidates.is_empty() {
            return None;
        }

        let current = self.active_window();
        let target = if candidates.len() == 1 {
            candidates[0]
        } else if let Some(index) = current.and_then(|id| candidates.iter().position(|item| *item == id)) {
            if forward {
                candidates[(index + 1) % candidates.len()]
            } else if index == 0 {
                candidates[candidates.len() - 1]
            } else {
                candidates[index - 1]
            }
        } else {
            candidates[0]
        };

        if self.focus(target) { Some(target) } else { None }
    }

    pub fn alt_tab_forward(&mut self) -> Option<WindowId> {
        self.alt_tab(true)
    }

    pub fn alt_tab_backward(&mut self) -> Option<WindowId> {
        self.alt_tab(false)
    }

    pub fn cycle_focus(&mut self, forward: bool) -> Option<WindowId> {
        self.alt_tab(forward)
    }

    /// Start a preview cycle without changing focus. Repeated calls to
    /// [`Self::cycle_tab`] preview candidates and may be committed or canceled
    /// by the keyboard integration.
    pub fn begin_tab_cycle(&mut self) -> Option<WindowId> {
        if let Some(cycle) = &self.tab_cycle {
            return cycle.candidates.get(cycle.index).copied();
        }
        let candidates = self.focusable_mru_ids();
        let origin = self.active_window().or_else(|| candidates.first().copied())?;
        let index = candidates.iter().position(|id| *id == origin).unwrap_or(0);
        self.tab_cycle = Some(TabCycle {
            origin,
            candidates,
            index,
        });
        Some(origin)
    }

    pub fn cycle_tab(&mut self, forward: bool) -> Option<WindowId> {
        if self.tab_cycle.is_none() {
            self.begin_tab_cycle()?;
        }
        let target = {
            let cycle = self.tab_cycle.as_mut()?;
            if cycle.candidates.len() > 1 {
                if forward {
                    cycle.index = (cycle.index + 1) % cycle.candidates.len();
                } else if cycle.index == 0 {
                    cycle.index = cycle.candidates.len() - 1;
                } else {
                    cycle.index -= 1;
                }
            }
            cycle.candidates[cycle.index]
        };
        if self.focus(target) {
            Some(target)
        } else {
            self.tab_cycle = None;
            self.focus_topmost_visible();
            None
        }
    }

    pub fn commit_tab_cycle(&mut self) -> Option<WindowId> {
        self.tab_cycle.take();
        self.active_window()
    }

    pub fn cancel_tab_cycle(&mut self) -> bool {
        let Some(cycle) = self.tab_cycle.take() else {
            return false;
        };
        if self.focus(cycle.origin) {
            true
        } else {
            self.focus_topmost_visible();
            false
        }
    }

    /// Visible, non-minimized windows in MRU order for an overview UI.
    pub fn overview_windows(&self) -> Vec<WindowId> {
        self.focusable_mru_ids()
    }

    pub fn overview_items(&self) -> Vec<WindowId> {
        self.overview_windows()
    }

    pub fn enter_overview(&mut self) -> bool {
        if self.overview {
            return false;
        }
        self.overview = true;
        true
    }

    pub fn exit_overview(&mut self) -> bool {
        if !self.overview {
            return false;
        }
        self.overview = false;
        true
    }

    pub fn toggle_overview(&mut self) -> bool {
        self.overview = !self.overview;
        self.overview
    }

    pub fn select_overview(&mut self, id: WindowId) -> bool {
        if !self.overview_windows().contains(&id) {
            return false;
        }
        self.overview = false;
        self.focus(id)
    }

    /// Marks the user intent to close. The client remains alive until it
    /// acknowledges the close by destroying its XDG toplevel.
    pub fn request_close(&mut self, id: WindowId) -> bool {
        let Some(window) = self.window_mut(id) else {
            return false;
        };
        if window.close_requested {
            false
        } else {
            window.close_requested = true;
            true
        }
    }

    /// Removes a toplevel only after the client destroyed it.
    pub fn remove(&mut self, id: WindowId) -> Option<Window> {
        let index = self.windows.iter().position(|window| window.id == id)?;
        let removed = self.windows.remove(index);
        self.mru.retain(|item| *item != id);
        self.tab_cycle = None;
        if removed.active || self.active_window().is_none() {
            self.focus_topmost_visible();
        }
        Some(removed)
    }

    /// Clamp a window's current frame and its normal restore frame. This is
    /// useful after a backend detects an output/scale change without replacing
    /// the work area itself.
    pub fn recover_window(&mut self, id: WindowId) -> Option<Rect> {
        let area = self.effective_work_area();
        let index = self.windows.iter().position(|window| window.id == id)?;
        let window = &mut self.windows[index];
        window.normal_frame = clamp_frame_to_area(area, window.normal_frame);
        if window.maximized || window.fullscreen {
            window.frame = area;
        } else if let Some(snap) = window.snap {
            window.frame = fit_rect_to_region(area, snap.rect(area), window.limits);
        } else {
            window.frame = clamp_frame_to_area(area, window.frame);
        }
        Some(window.frame)
    }

    /// Recover only windows whose visible frame changed. The returned list is
    /// convenient for an integration that needs to send configure requests.
    pub fn recover_offscreen_windows(&mut self) -> Vec<(WindowId, Rect)> {
        let ids = self.windows.iter().map(Window::id).collect::<Vec<_>>();
        let mut recovered = Vec::new();
        for id in ids {
            let before = self.window(id).map(Window::frame);
            let Some(after) = self.recover_window(id) else {
                continue;
            };
            if before != Some(after) {
                recovered.push((id, after));
            }
        }
        recovered
    }

    pub fn is_window_reachable(&self, id: WindowId) -> bool {
        self.window(id)
            .is_some_and(|window| window.frame == self.clamp_frame(window.frame))
    }

    /// A cheap public invariant check useful to shell/debug integrations.
    /// There is at most one active visible window, every window appears once
    /// in MRU, and the render stack is sorted by layer.
    pub fn invariants_hold(&self) -> bool {
        let active = self.windows.iter().filter(|window| window.active).count();
        if active > 1
            || self
                .windows
                .iter()
                .any(|window| window.active && !self.is_visible(window))
        {
            return false;
        }

        if self.windows.windows(2).any(|pair| pair[0].layer > pair[1].layer) {
            return false;
        }

        if self.mru.len() != self.windows.len() {
            return false;
        }
        for (index, id) in self.mru.iter().enumerate() {
            if self.window(*id).is_none() || self.mru[..index].contains(id) {
                return false;
            }
        }
        self.windows.iter().all(|window| self.mru.contains(&window.id))
    }

    fn effective_work_area(&self) -> Rect {
        Rect::new(
            self.work_area.origin.x,
            self.work_area.origin.y,
            self.work_area.size.width.max(1),
            self.work_area.size.height.max(1),
        )
    }

    fn clamp_frame(&self, frame: Rect) -> Rect {
        clamp_frame_to_area(self.effective_work_area(), frame)
    }

    fn is_visible(&self, window: &Window) -> bool {
        window.visible_on(self.current_workspace)
    }

    fn focusable_mru_ids(&self) -> Vec<WindowId> {
        let mut ids = Vec::with_capacity(self.windows.len());
        for id in &self.mru {
            if self.window(*id).is_some_and(|window| self.is_visible(window)) {
                ids.push(*id);
            }
        }
        // Keep the method total even if a future mutation forgets to update
        // MRU. It also makes recovery/debugging less surprising.
        for window in &self.windows {
            if self.is_visible(window) && !ids.contains(&window.id) {
                ids.push(window.id);
            }
        }
        ids
    }

    fn touch_mru(&mut self, id: WindowId) {
        self.mru.retain(|item| *item != id);
        self.mru.insert(0, id);
    }

    fn normalize_stack(&mut self) {
        self.windows.sort_by_key(|window| window.layer);
    }

    fn ensure_focus_for_workspace(&mut self) {
        if self.active_window().is_none() {
            self.clear_focus();
            self.focus_topmost_visible();
        }
    }

    fn focus_topmost_visible(&mut self) {
        let next = self
            .windows
            .iter()
            .rev()
            .find(|window| self.is_visible(window))
            .map(Window::id);
        if let Some(id) = next {
            let _ = self.focus(id);
        } else {
            self.clear_focus();
        }
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|window| window.id == id)
    }
}

fn clamp_origin_to_area(area: Rect, size: Size, position: Point) -> Point {
    let width = size.width.max(1);
    let height = size.height.max(1);
    let min_x = area.origin.x.saturating_sub(width).saturating_add(RETENTION);
    let max_x = area.right().saturating_sub(RETENTION);
    let min_y = area.origin.y.saturating_sub(height).saturating_add(RETENTION);
    let max_y = area.bottom().saturating_sub(RETENTION);

    Point::new(
        position.x.clamp(min_x.min(max_x), min_x.max(max_x)),
        position.y.clamp(min_y.min(max_y), min_y.max(max_y)),
    )
}

fn clamp_frame_to_area(area: Rect, frame: Rect) -> Rect {
    let size = Size::new(frame.size.width.max(1), frame.size.height.max(1));
    Rect {
        origin: clamp_origin_to_area(area, size, frame.origin),
        size,
    }
}

fn fit_rect_to_region(area: Rect, region: Rect, limits: SizeLimits) -> Rect {
    let requested = Size::new(region.size.width.max(1), region.size.height.max(1));
    let size = limits.constrain(requested);
    let x = region.origin.x.saturating_add(
        requested
            .width
            .saturating_sub(size.width)
            .checked_div(2)
            .unwrap_or(0),
    );
    let y = region.origin.y.saturating_add(
        requested
            .height
            .saturating_sub(size.height)
            .checked_div(2)
            .unwrap_or(0),
    );
    let frame = Rect {
        origin: Point::new(x, y),
        size,
    };
    // Normal snap cells fit in the area already. Clamping here handles a
    // minimum size larger than a tiny output and keeps its title bar usable.
    clamp_frame_to_area(area, frame)
}

fn partition(total: i32, parts: usize, index: usize) -> (i32, i32) {
    let parts = parts.max(1);
    let base = total / parts as i32;
    let remainder = (total % parts as i32).max(0);
    let size = base + i32::from(index < remainder as usize);
    let offset = base.saturating_mul(index as i32) + (index.min(remainder as usize) as i32);
    (offset, size.max(1))
}

fn tile_regions(area: Rect, count: usize, layout: TileLayout) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let width = area.size.width.max(1);
    let height = area.size.height.max(1);
    match layout {
        TileLayout::Columns => (0..count)
            .map(|index| {
                let (x, cell_width) = partition(width, count, index);
                Rect::new(area.origin.x.saturating_add(x), area.origin.y, cell_width, height)
            })
            .collect(),
        TileLayout::Rows => (0..count)
            .map(|index| {
                let (y, cell_height) = partition(height, count, index);
                Rect::new(area.origin.x, area.origin.y.saturating_add(y), width, cell_height)
            })
            .collect(),
        TileLayout::Grid => {
            let mut columns = 1usize;
            while columns.saturating_mul(columns) < count {
                columns += 1;
            }
            let rows = count.div_ceil(columns);
            (0..count)
                .map(|index| {
                    let row = index / columns;
                    let column = index % columns;
                    let (x, cell_width) = partition(width, columns, column);
                    let (y, cell_height) = partition(height, rows, row);
                    Rect::new(
                        area.origin.x.saturating_add(x),
                        area.origin.y.saturating_add(y),
                        cell_width,
                        cell_height,
                    )
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(manager: &WindowManager) -> WindowId {
        manager.active_window().expect("a window should be active")
    }

    #[test]
    fn minimizing_the_front_window_activates_the_next_visible_window() {
        let mut manager = WindowManager::default();
        let first = manager.create_window(Size::new(640, 480), SizeLimits::default());
        let second = manager.create_window(Size::new(640, 480), SizeLimits::default());

        assert!(manager.window(second).unwrap().active());
        assert!(manager.minimize(second));
        assert!(manager.window(second).unwrap().minimized());
        assert!(manager.window(first).unwrap().active());

        assert!(manager.restore(second));
        assert!(manager.window(second).unwrap().active());
    }

    #[test]
    fn maximize_preserves_the_normal_frame() {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());
        let normal = manager.window(id).unwrap().frame();

        assert!(manager.toggle_maximized(id));
        assert_eq!(manager.window(id).unwrap().frame(), DEFAULT_WORK_AREA);
        assert!(
            manager
                .resize_by(id, ResizeEdge::Right, Point { x: 100, y: 0 })
                .is_none()
        );

        assert!(manager.toggle_maximized(id));
        assert_eq!(manager.window(id).unwrap().frame(), normal);
    }

    #[test]
    fn repeated_xdg_maximize_requests_are_idempotent() {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());

        assert!(manager.maximize(id));
        assert!(manager.maximize(id));
        assert!(manager.window(id).unwrap().maximized());
        assert_eq!(manager.window(id).unwrap().frame(), DEFAULT_WORK_AREA);
    }

    #[test]
    fn fullscreen_restores_the_previous_maximized_state() {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());
        assert!(manager.toggle_maximized(id));

        assert!(manager.enter_fullscreen(id));
        assert!(manager.window(id).unwrap().fullscreen());
        assert!(manager.exit_fullscreen(id));
        assert!(manager.window(id).unwrap().maximized());
        assert_eq!(manager.window(id).unwrap().frame(), DEFAULT_WORK_AREA);
    }

    #[test]
    fn resizing_from_left_keeps_the_right_edge_in_place() {
        let mut manager = WindowManager::default();
        let limits = SizeLimits {
            minimum: Size::new(500, 300),
            maximum: None,
        };
        let id = manager.create_window(Size::new(640, 480), limits);
        let before = manager.window(id).unwrap().frame();

        let resized = manager
            .resize_by(id, ResizeEdge::Left, Point { x: 1000, y: 0 })
            .unwrap();
        assert_eq!(resized.size.width, 500);
        assert_eq!(resized.right(), before.right());
    }

    #[test]
    fn a_destroyed_active_window_hands_focus_to_the_next_window() {
        let mut manager = WindowManager::default();
        let first = manager.create_window(Size::new(640, 480), SizeLimits::default());
        let second = manager.create_window(Size::new(640, 480), SizeLimits::default());

        assert!(manager.remove(second).is_some());
        assert!(manager.window(first).unwrap().active());
    }

    #[test]
    fn move_to_never_lets_a_window_escape_the_work_area() {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());

        let flung = manager.move_to(id, Point { x: -5000, y: -5000 }).unwrap();
        assert!(manager.work_area().right() - flung.origin.x >= RETENTION);
        assert!(flung.origin.x <= manager.work_area().right());

        let far_right = manager.move_to(id, Point { x: 5000, y: 0 }).unwrap();
        assert!(far_right.right() >= manager.work_area().origin.x + RETENTION);
        assert!(far_right.origin.x < manager.work_area().right());

        let sane = manager.move_to(id, Point { x: 300, y: 200 }).unwrap();
        assert_eq!(sane, Rect::new(300, 200, 640, 480));

        // A huge window is still movable: its top-left leaves the area, but
        // the retained title-bar strip stays reachable.
        let huge = manager.create_window(Size::new(4000, 4000), SizeLimits::default());
        let clamped = manager.move_to(huge, Point { x: -9999, y: -9999 }).unwrap();
        assert!(clamped.right() >= manager.work_area().origin.x + RETENTION);
        assert!(clamped.bottom() >= manager.work_area().origin.y + RETENTION);
    }

    #[test]
    fn move_to_is_blocked_for_special_windows() {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());
        assert!(manager.maximize(id));
        assert!(manager.move_to(id, Point { x: 10, y: 10 }).is_none());

        let other = manager.create_window(Size::new(640, 480), SizeLimits::default());
        assert!(manager.minimize(other));
        assert!(manager.move_to(other, Point { x: 10, y: 10 }).is_none());
    }

    #[test]
    fn titles_are_set_and_reported() {
        let mut manager = WindowManager::default();
        let id = manager.create_window(Size::new(640, 480), SizeLimits::default());

        assert_eq!(manager.window(id).unwrap().title(), "");
        assert!(manager.set_title(id, "Terminal — Rouch"));
        assert_eq!(manager.window(id).unwrap().title(), "Terminal — Rouch");
        assert!(!manager.set_title(id, "Terminal — Rouch"));
    }

    #[test]
    fn focus_and_alt_tab_follow_mru_order() {
        let mut manager = WindowManager::default();
        let first = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let second = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let third = manager.create_window(Size::new(400, 300), SizeLimits::default());

        assert_eq!(active(&manager), third);
        assert!(manager.focus(first));
        assert_eq!(manager.mru()[0], first);
        assert_eq!(manager.alt_tab(true), Some(third));
        assert_eq!(manager.alt_tab(true), Some(first));
        assert_eq!(manager.alt_tab(false), Some(second));
        assert_eq!(active(&manager), second);
        assert!(manager.invariants_hold());
    }

    #[test]
    fn tab_cycle_can_commit_or_restore_its_origin() {
        let mut manager = WindowManager::default();
        let first = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let second = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let third = manager.create_window(Size::new(400, 300), SizeLimits::default());

        assert_eq!(active(&manager), third);
        assert_eq!(manager.begin_tab_cycle(), Some(third));
        assert_eq!(manager.cycle_tab(true), Some(second));
        assert!(manager.tab_cycle_active());
        assert!(manager.cancel_tab_cycle());
        assert_eq!(active(&manager), third);

        assert_eq!(manager.begin_tab_cycle(), Some(third));
        assert_eq!(manager.cycle_tab(false), Some(first));
        assert_eq!(manager.commit_tab_cycle(), Some(first));
        assert!(!manager.tab_cycle_active());
    }

    #[test]
    fn layers_stay_sorted_and_pinned_windows_cross_workspaces() {
        let mut manager = WindowManager::new(Rect::new(0, 0, 1000, 800));
        let normal = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let utility = manager.create_window(Size::new(300, 200), SizeLimits::default());
        assert!(manager.set_always_on_top(normal, true));
        assert!(manager.focus(utility));
        assert_eq!(manager.windows().last().unwrap().id(), normal);

        assert!(manager.set_pinned(normal, true));
        let workspace = manager.create_workspace();
        assert!(manager.move_to_workspace(utility, workspace));
        assert!(manager.switch_workspace(workspace));
        assert!(manager.visible_window_ids().contains(&normal));
        assert!(manager.visible_window_ids().contains(&utility));
        assert_eq!(manager.window(normal).unwrap().layer(), WindowLayer::AlwaysOnTop);
        assert!(manager.invariants_hold());
    }

    #[test]
    fn workspace_switch_picks_a_visible_window_and_skips_hidden_mru() {
        let mut manager = WindowManager::default();
        let first = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let second = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let workspace = manager.create_workspace();
        assert!(manager.move_to_workspace(second, workspace));

        assert!(manager.switch_workspace(workspace));
        assert_eq!(active(&manager), second);
        assert!(manager.switch_workspace(WorkspaceId::new(1)));
        assert_eq!(active(&manager), first);
    }

    #[test]
    fn overview_exposes_current_mru_and_selects_a_window() {
        let mut manager = WindowManager::default();
        let first = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let second = manager.create_window(Size::new(400, 300), SizeLimits::default());
        assert!(manager.enter_overview());
        assert!(manager.overview_active());
        assert_eq!(manager.overview_windows(), vec![second, first]);
        assert!(manager.select_overview(first));
        assert!(!manager.overview_active());
        assert_eq!(active(&manager), first);
    }

    #[test]
    fn snap_unsnap_and_tiling_keep_restore_geometry() {
        let area = Rect::new(10, 20, 1000, 800);
        let mut manager = WindowManager::new(area);
        let first = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let second = manager.create_window(Size::new(400, 300), SizeLimits::default());
        let normal = manager.window(first).unwrap().normal_frame();

        assert_eq!(
            manager.snap_window(first, SnapPosition::Left),
            Some(SnapPosition::Left.rect(area))
        );
        assert!(manager.window(first).unwrap().snapped());
        assert!(manager.unsnap(first));
        assert_eq!(manager.window(first).unwrap().frame(), normal);

        let tiled = manager.tile_workspace(TileLayout::Columns);
        assert_eq!(tiled.len(), 2);
        assert!(manager.window(first).unwrap().tiled());
        assert!(manager.window(second).unwrap().tiled());
        assert_eq!(tiled[0].1.size.width + tiled[1].1.size.width, area.size.width);
        assert_eq!(manager.untile_workspace(), 2);
        assert!(!manager.window(first).unwrap().tiled());
    }

    #[test]
    fn recovery_repairs_normal_and_special_frames_after_output_change() {
        let mut manager = WindowManager::new(Rect::new(0, 0, 1200, 800));
        let id = manager.create_window(Size::new(500, 400), SizeLimits::default());
        assert_eq!(
            manager.move_to(id, Point::new(-5000, -5000)).unwrap().origin.x,
            -404
        );

        manager.set_work_area(Rect::new(100, 50, 800, 600));
        assert!(manager.is_window_reachable(id));
        assert!(manager.recover_offscreen_windows().is_empty());

        assert!(manager.maximize(id));
        manager.set_work_area(Rect::new(-20, -10, 640, 480));
        assert_eq!(manager.window(id).unwrap().frame(), manager.work_area());
        assert!(manager.is_window_reachable(id));
    }

    #[test]
    fn limits_never_allow_invalid_or_reversed_dimensions() {
        let limits = SizeLimits {
            minimum: Size::new(500, 300),
            maximum: Some(Size::new(100, 100)),
        };
        assert_eq!(limits.constrain(Size::new(-1, -1)), Size::new(500, 300));

        let mut manager = WindowManager::new(Rect::new(0, 0, 900, 600));
        let id = manager.create_window(Size::new(400, 200), limits);
        assert_eq!(manager.window(id).unwrap().frame().size, Size::new(500, 300));
        let frame = manager
            .resize_by(id, ResizeEdge::Right, Point::new(-1000, -1000))
            .unwrap();
        assert_eq!(frame.size, Size::new(500, 300));
        assert!(manager.invariants_hold());
    }
}
