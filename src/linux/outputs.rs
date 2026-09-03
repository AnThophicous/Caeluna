//! Output discovery, layout and native-session boundaries for Rouch.
//!
//! The model here is independent from a DRM device and can therefore be
//! exercised on Mint, Ubuntu, Arch, CI, or the existing nested backend.  On a
//! native session, [`SysfsOutputDiscovery`] reads the kernel's connector
//! state and [`UdevOutputSource`] turns Smithay 0.7 udev notifications into a
//! cheap rescan signal.  The rescan itself is intentionally idempotent: a
//! connector change never causes a stale output to remain focused or mapped.
//!
//! Geometry is expressed in logical pixels after scale.  This keeps the
//! compositor's window model stable for HiDPI displays and lets the existing
//! Liquid Glass shell choose its blur/opaque recipe independently.  No output
//! path assumes blur, Vulkan, or a particular number of monitors.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// Lowest accepted output scale.  Values outside the range are clamped so a
/// corrupt configuration cannot produce a zero-sized logical output.
pub const MIN_OUTPUT_SCALE: f64 = 0.5;

/// Highest accepted output scale.
pub const MAX_OUTPUT_SCALE: f64 = 4.0;

/// A display mode as exposed by the compositor model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputMode {
    /// Physical horizontal pixels.
    pub width: i32,
    /// Physical vertical pixels.
    pub height: i32,
    /// Refresh rate in millihertz, for example `60000` for 60 Hz.
    pub refresh_mhz: i32,
}

impl OutputMode {
    /// Construct a mode with safe positive bounds.
    pub fn new(width: i32, height: i32, refresh_mhz: i32) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
            refresh_mhz: refresh_mhz.max(1),
        }
    }

    /// Whether this mode is usable without further repair.
    pub fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0 && self.refresh_mhz > 0
    }

    /// Pixel area used for deterministic mode ranking.
    pub fn area(self) -> i64 {
        i64::from(self.width.max(0)) * i64::from(self.height.max(0))
    }
}

/// A requested size/rate from a user configuration or compositor policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeRequest {
    /// Desired physical width.
    pub width: i32,
    /// Desired physical height.
    pub height: i32,
    /// Optional desired refresh.  The closest supported rate wins.
    pub refresh_mhz: Option<i32>,
}

/// Pick a mode without ever returning an unsupported mode.
///
/// Exact requested dimensions win.  Within those dimensions the nearest
/// refresh is selected; if the request is absent, the preferred mode, current
/// mode, and then the largest available mode are used in that order.  This
/// makes hotplug recovery deterministic and avoids a black screen caused by a
/// stale saved mode.
pub fn select_mode(
    modes: &[OutputMode],
    preferred: Option<OutputMode>,
    current: Option<OutputMode>,
    request: Option<ModeRequest>,
) -> Option<OutputMode> {
    let valid = modes.iter().copied().filter(|mode| mode.is_valid());
    let modes = valid.collect::<Vec<_>>();
    if modes.is_empty() {
        return None;
    }

    if let Some(request) = request {
        let mut matching = modes
            .iter()
            .copied()
            .filter(|mode| mode.width == request.width && mode.height == request.height);
        if let Some(first) = matching.next() {
            return Some(matching.fold(first, |best, candidate| {
                let Some(wanted) = request.refresh_mhz else {
                    return if candidate.refresh_mhz > best.refresh_mhz {
                        candidate
                    } else {
                        best
                    };
                };
                let best_distance = (best.refresh_mhz - wanted).unsigned_abs();
                let candidate_distance = (candidate.refresh_mhz - wanted).unsigned_abs();
                if candidate_distance < best_distance
                    || (candidate_distance == best_distance && candidate.refresh_mhz > best.refresh_mhz)
                {
                    candidate
                } else {
                    best
                }
            }));
        }
    }

    preferred
        .filter(|mode| modes.contains(mode))
        .or_else(|| current.filter(|mode| modes.contains(mode)))
        .or_else(|| {
            modes
                .into_iter()
                .max_by_key(|mode| (mode.area(), mode.refresh_mhz))
        })
}

/// Rotation/flip state advertised to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutputTransform {
    /// No transform.
    #[default]
    Normal,
    /// Rotate clockwise by 90 degrees.
    Rotate90,
    /// Rotate by 180 degrees.
    Rotate180,
    /// Rotate clockwise by 270 degrees.
    Rotate270,
    /// Flip vertically.
    Flipped,
    /// Flip and rotate clockwise by 90 degrees.
    Flipped90,
    /// Flip and rotate by 180 degrees.
    Flipped180,
    /// Flip and rotate clockwise by 270 degrees.
    Flipped270,
}

/// A logical point in the global compositor space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LogicalPoint {
    /// Horizontal position.
    pub x: i32,
    /// Vertical position.
    pub y: i32,
}

/// A logical output rectangle in the global compositor space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OutputGeometry {
    /// Horizontal position.
    pub x: i32,
    /// Vertical position.
    pub y: i32,
    /// Logical width.
    pub width: i32,
    /// Logical height.
    pub height: i32,
}

impl OutputGeometry {
    /// Construct a rectangle while preventing negative dimensions.
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// Right edge in global coordinates.
    pub fn right(self) -> i32 {
        self.x.saturating_add(self.width)
    }

    /// Bottom edge in global coordinates.
    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.height)
    }

    /// Center point used by directional output focus.
    pub fn center(self) -> (i64, i64) {
        (
            i64::from(self.x) + i64::from(self.width) / 2,
            i64::from(self.y) + i64::from(self.height) / 2,
        )
    }

    /// Whether a global point falls inside this output.
    pub fn contains(self, point: LogicalPoint) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// Union with another rectangle, preserving a one-pixel safe fallback.
    pub fn union(self, other: Self) -> Self {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::new(left, top, right.saturating_sub(left), bottom.saturating_sub(top))
    }
}

/// A display snapshot produced by sysfs, DRM, or a nested host backend.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSnapshot {
    /// Stable connector identifier, such as `card0-HDMI-A-1`.
    pub id: String,
    /// User-visible connector name.
    pub name: String,
    /// EDID make when available.
    pub make: String,
    /// EDID model when available.
    pub model: String,
    /// Physical dimensions in millimeters, if known.
    pub physical_mm: Option<(i32, i32)>,
    /// Modes currently reported by the connector.
    pub modes: Vec<OutputMode>,
    /// Kernel/driver current mode when known.
    pub current_mode: Option<OutputMode>,
    /// Driver preferred mode when known.
    pub preferred_mode: Option<OutputMode>,
    /// Logical scale requested for this output.
    pub scale: f64,
    /// Rotation/flip requested for this output.
    pub transform: OutputTransform,
    /// Optional user-preserved location.  The layout policy may replace it.
    pub position: Option<LogicalPoint>,
    /// Whether this connector is the chosen primary output.
    pub primary: bool,
}

impl OutputSnapshot {
    /// Make a minimal connected output snapshot.
    pub fn new(id: impl Into<String>, modes: impl IntoIterator<Item = OutputMode>) -> Self {
        let id = id.into();
        let modes = modes.into_iter().collect::<Vec<_>>();
        let preferred_mode = modes.first().copied();
        Self {
            name: id.clone(),
            id,
            make: String::new(),
            model: String::new(),
            physical_mm: None,
            modes,
            current_mode: preferred_mode,
            preferred_mode,
            scale: 1.0,
            transform: OutputTransform::Normal,
            position: None,
            primary: false,
        }
    }

    /// Repair values from an unreliable discovery source.
    pub fn sanitized(mut self) -> Self {
        self.id = self.id.trim().to_owned();
        if self.name.trim().is_empty() {
            self.name = self.id.clone();
        }
        self.name = self.name.trim().to_owned();
        self.modes.retain(|mode| mode.is_valid());
        self.modes
            .sort_by_key(|mode| (mode.width, mode.height, mode.refresh_mhz));
        self.modes.dedup();
        self.scale = sanitize_scale(self.scale);
        self.preferred_mode = self.preferred_mode.filter(|mode| self.modes.contains(mode));
        self.current_mode = self.current_mode.filter(|mode| self.modes.contains(mode));
        if self.preferred_mode.is_none() {
            self.preferred_mode = self.modes.first().copied();
        }
        if self.current_mode.is_none() {
            self.current_mode = select_mode(&self.modes, self.preferred_mode, None, None);
        }
        self.physical_mm = self
            .physical_mm
            .filter(|(width, height)| *width >= 0 && *height >= 0);
        self
    }

    /// Return the selected physical mode after safe fallback.
    pub fn selected_mode(&self) -> Option<OutputMode> {
        select_mode(&self.modes, self.preferred_mode, self.current_mode, None)
    }

    /// Convert the selected physical mode to logical geometry.
    pub fn logical_size(&self) -> (i32, i32) {
        let mode = self.selected_mode().unwrap_or_else(|| OutputMode::new(1, 1, 1));
        let width = (f64::from(mode.width) / self.scale).ceil() as i32;
        let height = (f64::from(mode.height) / self.scale).ceil() as i32;
        (width.max(1), height.max(1))
    }

    /// Convert this snapshot to Smithay's public output abstraction.
    ///
    /// This does not modeset a DRM connector; the native backend owns that
    /// operation.  It prepares the `wl_output` state used by Smithay's
    /// `OutputManagerState` and by the existing desktop space.
    pub fn smithay_output(&self) -> smithay::output::Output {
        let physical = self.physical_mm.unwrap_or((0, 0));
        let output = smithay::output::Output::new(
            self.id.clone(),
            smithay::output::PhysicalProperties {
                size: (physical.0, physical.1).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: if self.make.is_empty() {
                    "Unknown".into()
                } else {
                    self.make.clone()
                },
                model: if self.model.is_empty() {
                    self.name.clone()
                } else {
                    self.model.clone()
                },
            },
        );
        for mode in &self.modes {
            output.add_mode(smithay_mode(*mode));
        }
        if let Some(mode) = self.preferred_mode {
            output.set_preferred(smithay_mode(mode));
        }
        let current = self.selected_mode().map(smithay_mode);
        let location = self.position.unwrap_or_default();
        output.change_current_state(
            current,
            Some(smithay_transform(self.transform)),
            Some(smithay::output::Scale::Fractional(self.scale)),
            Some((location.x, location.y).into()),
        );
        output
    }

    /// Apply a changed snapshot to an existing Smithay output handle.
    pub fn configure_smithay_output(&self, output: &smithay::output::Output) {
        for mode in &self.modes {
            output.add_mode(smithay_mode(*mode));
        }
        if let Some(mode) = self.preferred_mode {
            output.set_preferred(smithay_mode(mode));
        }
        let location = self.position.unwrap_or_default();
        output.change_current_state(
            self.selected_mode().map(smithay_mode),
            Some(smithay_transform(self.transform)),
            Some(smithay::output::Scale::Fractional(self.scale)),
            Some((location.x, location.y).into()),
        );
    }
}

/// Clamp a scale to a valid logical range.
pub fn sanitize_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(MIN_OUTPUT_SCALE, MAX_OUTPUT_SCALE)
    } else {
        1.0
    }
}

fn smithay_mode(mode: OutputMode) -> smithay::output::Mode {
    smithay::output::Mode {
        size: (mode.width, mode.height).into(),
        refresh: mode.refresh_mhz,
    }
}

fn smithay_transform(transform: OutputTransform) -> smithay::utils::Transform {
    match transform {
        OutputTransform::Normal => smithay::utils::Transform::Normal,
        OutputTransform::Rotate90 => smithay::utils::Transform::_90,
        OutputTransform::Rotate180 => smithay::utils::Transform::_180,
        OutputTransform::Rotate270 => smithay::utils::Transform::_270,
        OutputTransform::Flipped => smithay::utils::Transform::Flipped,
        OutputTransform::Flipped90 => smithay::utils::Transform::Flipped90,
        OutputTransform::Flipped180 => smithay::utils::Transform::Flipped180,
        OutputTransform::Flipped270 => smithay::utils::Transform::Flipped270,
    }
}

/// Arrangement policy for the global output space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutStrategy {
    /// Place outputs left-to-right, primary first.
    Horizontal,
    /// Place outputs top-to-bottom, primary first.
    Vertical,
    /// Keep saved locations and append only outputs without a location.
    Preserve,
}

/// A rectangle assigned to one output after arrangement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlacement {
    /// Connector identifier.
    pub id: String,
    /// Assigned global logical geometry.
    pub geometry: OutputGeometry,
}

/// Result of a layout pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutSummary {
    /// All assigned placements in deterministic order.
    pub placements: Vec<OutputPlacement>,
    /// Bounding rectangle of the virtual desktop.
    pub virtual_geometry: Option<OutputGeometry>,
}

/// State transition generated by output discovery/session reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputChange {
    /// A connector became available.
    Added(String),
    /// A connector's mode, scale, or geometry changed.
    Changed(String),
    /// A connector disappeared.
    Removed(String),
    /// The keyboard/pointer output focus changed.
    FocusChanged(Option<String>),
    /// The native session changed lifecycle state.
    SessionChanged(SessionState),
}

/// Native session lifecycle as seen by the output model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The compositor owns the active VT.
    Active {
        /// Active virtual terminal number when known.
        vt: Option<i32>,
    },
    /// The compositor must not submit to DRM or deliver input.
    Paused,
    /// The seat connection was lost and needs a controlled restart.
    Lost,
}

/// A validated request to change virtual terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtRequest {
    /// Target VT number, one through 63.
    pub vt: i32,
}

/// Errors from the pure output/session state boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputError {
    /// A VT number outside the Linux range was requested.
    InvalidVt(i32),
    /// A VT switch was requested while the seat was not active.
    SessionInactive,
    /// A requested output is not currently connected.
    UnknownOutput(String),
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidVt(vt) => write!(f, "invalid VT {vt}; expected 1..=63"),
            Self::SessionInactive => f.write_str("output session is inactive"),
            Self::UnknownOutput(id) => write!(f, "unknown output {id}"),
        }
    }
}

impl std::error::Error for OutputError {}

#[derive(Debug, Clone)]
struct OutputRecord {
    snapshot: OutputSnapshot,
    geometry: OutputGeometry,
}

/// Deterministic multi-output registry used by native and nested backends.
#[derive(Debug, Clone)]
pub struct OutputRegistry {
    outputs: BTreeMap<String, OutputRecord>,
    focused: Option<String>,
    saved_focus: Option<String>,
    session: SessionState,
}

impl Default for OutputRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputRegistry {
    /// Create an empty active registry.
    pub fn new() -> Self {
        Self {
            outputs: BTreeMap::new(),
            focused: None,
            saved_focus: None,
            session: SessionState::Active { vt: None },
        }
    }

    /// Current session lifecycle.
    pub fn session_state(&self) -> SessionState {
        self.session
    }

    /// Current focused connector, if any.
    pub fn focused_output(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    /// Iterate output snapshots in stable connector-id order.
    pub fn snapshots(&self) -> impl Iterator<Item = &OutputSnapshot> {
        self.outputs.values().map(|record| &record.snapshot)
    }

    /// Get one snapshot by connector id.
    pub fn snapshot(&self, id: &str) -> Option<&OutputSnapshot> {
        self.outputs.get(id).map(|record| &record.snapshot)
    }

    /// Get one assigned geometry by connector id.
    pub fn geometry(&self, id: &str) -> Option<OutputGeometry> {
        self.outputs.get(id).map(|record| record.geometry)
    }

    /// Replace the connected-output set and emit minimal changes.
    pub fn reconcile(
        &mut self,
        snapshots: impl IntoIterator<Item = OutputSnapshot>,
        strategy: LayoutStrategy,
    ) -> Vec<OutputChange> {
        let mut incoming = BTreeMap::new();
        for snapshot in snapshots {
            let snapshot = snapshot.sanitized();
            if !snapshot.id.is_empty() {
                incoming.insert(snapshot.id.clone(), snapshot);
            }
        }

        let mut changes = Vec::new();
        for (id, snapshot) in &incoming {
            match self.outputs.get_mut(id) {
                Some(record) => {
                    if record.snapshot != *snapshot {
                        record.snapshot = snapshot.clone();
                        changes.push(OutputChange::Changed(id.clone()));
                    }
                }
                None => {
                    self.outputs.insert(
                        id.clone(),
                        OutputRecord {
                            geometry: geometry_for_snapshot(snapshot),
                            snapshot: snapshot.clone(),
                        },
                    );
                    changes.push(OutputChange::Added(id.clone()));
                }
            }
        }

        let removed = self
            .outputs
            .keys()
            .filter(|id| !incoming.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in removed {
            self.outputs.remove(&id);
            changes.push(OutputChange::Removed(id));
        }

        self.arrange(strategy);
        if !matches!(self.session, SessionState::Active { .. }) {
            self.focused = None;
        } else if self
            .focused
            .as_ref()
            .is_none_or(|id| !self.outputs.contains_key(id))
        {
            let previous = self.focused.clone();
            self.focused = self.first_output_id();
            if previous != self.focused {
                changes.push(OutputChange::FocusChanged(self.focused.clone()));
            }
        }
        changes
    }

    /// Arrange current outputs and return their global geometry.
    pub fn arrange(&mut self, strategy: LayoutStrategy) -> LayoutSummary {
        let ids = self.ordered_ids();
        let mut placements = Vec::with_capacity(ids.len());
        let mut cursor_x = 0i32;
        let mut cursor_y = 0i32;
        let mut virtual_geometry: Option<OutputGeometry> = None;

        for id in ids {
            let Some(record) = self.outputs.get(&id) else {
                continue;
            };
            let (width, height) = record.snapshot.logical_size();
            let geometry = match strategy {
                LayoutStrategy::Horizontal => {
                    let geometry = OutputGeometry::new(cursor_x, 0, width, height);
                    cursor_x = cursor_x.saturating_add(width);
                    geometry
                }
                LayoutStrategy::Vertical => {
                    let geometry = OutputGeometry::new(0, cursor_y, width, height);
                    cursor_y = cursor_y.saturating_add(height);
                    geometry
                }
                LayoutStrategy::Preserve => record
                    .snapshot
                    .position
                    .map(|position| OutputGeometry::new(position.x, position.y, width, height))
                    .unwrap_or_else(|| {
                        let geometry = OutputGeometry::new(cursor_x, 0, width, height);
                        cursor_x = cursor_x.saturating_add(width);
                        geometry
                    }),
            };

            if let Some(current) = virtual_geometry {
                virtual_geometry = Some(current.union(geometry));
            } else {
                virtual_geometry = Some(geometry);
            }
            placements.push(OutputPlacement { id, geometry });
        }

        for placement in &placements {
            if let Some(record) = self.outputs.get_mut(&placement.id) {
                record.geometry = placement.geometry;
            }
        }
        LayoutSummary {
            placements,
            virtual_geometry,
        }
    }

    /// Focus one connected output.
    pub fn focus_output(&mut self, id: &str) -> Result<Option<OutputChange>, OutputError> {
        if !self.outputs.contains_key(id) {
            return Err(OutputError::UnknownOutput(id.to_owned()));
        }
        if !matches!(self.session, SessionState::Active { .. }) {
            return Err(OutputError::SessionInactive);
        }
        if self.focused.as_deref() == Some(id) {
            return Ok(None);
        }
        self.focused = Some(id.to_owned());
        Ok(Some(OutputChange::FocusChanged(self.focused.clone())))
    }

    /// Focus the next/previous output in deterministic connector order.
    pub fn focus_next(&mut self, reverse: bool) -> Option<OutputChange> {
        let ids = self.ordered_ids();
        if ids.is_empty() || !matches!(self.session, SessionState::Active { .. }) {
            return None;
        }
        let current = self
            .focused
            .as_ref()
            .and_then(|focused| ids.iter().position(|id| id == focused))
            .unwrap_or(0);
        let next = if reverse {
            (current + ids.len() - 1) % ids.len()
        } else {
            (current + 1) % ids.len()
        };
        self.focused = Some(ids[next].clone());
        Some(OutputChange::FocusChanged(self.focused.clone()))
    }

    /// Focus the nearest output in a spatial direction, like macOS display
    /// navigation while keeping the current virtual desktop stable.
    pub fn focus_direction(&mut self, direction: OutputDirection) -> Option<OutputChange> {
        let current_id = self.focused.as_ref()?;
        let current = self.outputs.get(current_id)?.geometry;
        let current_center = current.center();
        let candidate = self
            .outputs
            .iter()
            .filter(|(id, _)| *id != current_id)
            .filter_map(|(id, record)| {
                let center = record.geometry.center();
                let dx = center.0 - current_center.0;
                let dy = center.1 - current_center.1;
                let in_direction = match direction {
                    OutputDirection::Left => dx < 0,
                    OutputDirection::Right => dx > 0,
                    OutputDirection::Up => dy < 0,
                    OutputDirection::Down => dy > 0,
                };
                if !in_direction {
                    return None;
                }
                let primary_distance = match direction {
                    OutputDirection::Left | OutputDirection::Right => dx.unsigned_abs(),
                    OutputDirection::Up | OutputDirection::Down => dy.unsigned_abs(),
                };
                let secondary_distance = match direction {
                    OutputDirection::Left | OutputDirection::Right => dy.unsigned_abs(),
                    OutputDirection::Up | OutputDirection::Down => dx.unsigned_abs(),
                };
                Some(((primary_distance, secondary_distance, id.as_str()), id.clone()))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, id)| id)?;
        self.focused = Some(candidate);
        Some(OutputChange::FocusChanged(self.focused.clone()))
    }

    /// Apply a seat pause/activation/loss and preserve a recoverable focus.
    pub fn session_event(&mut self, event: SessionEvent) -> Vec<OutputChange> {
        let mut changes = Vec::new();
        match event {
            SessionEvent::Pause => {
                self.saved_focus = self.focused.take();
                self.session = SessionState::Paused;
                changes.push(OutputChange::SessionChanged(self.session));
                changes.push(OutputChange::FocusChanged(None));
            }
            SessionEvent::Activate { vt } => {
                self.session = SessionState::Active { vt };
                changes.push(OutputChange::SessionChanged(self.session));
                let restored = self
                    .saved_focus
                    .take()
                    .filter(|id| self.outputs.contains_key(id))
                    .or_else(|| self.first_output_id());
                self.focused = restored;
                changes.push(OutputChange::FocusChanged(self.focused.clone()));
            }
            SessionEvent::Lost => {
                self.saved_focus = self.focused.take();
                self.session = SessionState::Lost;
                changes.push(OutputChange::SessionChanged(self.session));
                changes.push(OutputChange::FocusChanged(None));
            }
        }
        changes
    }

    /// Validate a requested VT switch before passing it to libseat.
    pub fn request_vt(&self, vt: i32) -> Result<VtRequest, OutputError> {
        if !(1..=63).contains(&vt) {
            return Err(OutputError::InvalidVt(vt));
        }
        if !matches!(self.session, SessionState::Active { .. }) {
            return Err(OutputError::SessionInactive);
        }
        Ok(VtRequest { vt })
    }

    fn first_output_id(&self) -> Option<String> {
        self.ordered_ids().into_iter().next()
    }

    fn ordered_ids(&self) -> Vec<String> {
        let mut ids = self.outputs.keys().cloned().collect::<Vec<_>>();
        ids.sort_by_key(|id| {
            let primary = self
                .outputs
                .get(id)
                .map(|record| !record.snapshot.primary)
                .unwrap_or(true);
            (primary, id.clone())
        });
        ids
    }
}

/// Direction for spatial output focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputDirection {
    /// Output to the left.
    Left,
    /// Output to the right.
    Right,
    /// Output above.
    Up,
    /// Output below.
    Down,
}

/// Pure session event consumed by [`OutputRegistry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// The VT/seat paused the compositor.
    Pause,
    /// The VT/seat became active again.
    Activate {
        /// Active VT when supplied by the session implementation.
        vt: Option<i32>,
    },
    /// The seat connection was lost.
    Lost,
}

fn geometry_for_snapshot(snapshot: &OutputSnapshot) -> OutputGeometry {
    let (width, height) = snapshot.logical_size();
    let position = snapshot.position.unwrap_or_default();
    OutputGeometry::new(position.x, position.y, width, height)
}

/// Scan connected DRM connectors through the kernel sysfs ABI.
///
/// This path is intentionally dependency-light and works before the full DRM
/// renderer is initialized.  It is a discovery source, not a modesetter: a
/// native compositor still validates and commits the chosen mode using
/// Smithay's DRM backend.
#[derive(Debug, Clone)]
pub struct SysfsOutputDiscovery {
    root: PathBuf,
}

impl Default for SysfsOutputDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

impl SysfsOutputDiscovery {
    /// Use `/sys/class/drm` as the live connector root.
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/sys/class/drm"),
        }
    }

    /// Use an alternate root for tests or a containerized compositor.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Scan currently connected connector directories.
    pub fn scan(&self) -> io::Result<Vec<OutputSnapshot>> {
        let mut entries = std::fs::read_dir(&self.root)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        let mut outputs = Vec::new();
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_connector_name(&name) {
                continue;
            }
            let connector = entry.path();
            if read_trimmed(&connector.join("status")).as_deref() != Some("connected") {
                continue;
            }
            let modes = read_trimmed(&connector.join("modes"))
                .as_deref()
                .map(parse_modes)
                .unwrap_or_default();
            if modes.is_empty() {
                tracing::debug!(connector = %name, "connected DRM connector exposed no usable mode");
                continue;
            }
            let current_mode = read_trimmed(&connector.join("mode"))
                .as_deref()
                .and_then(parse_mode);
            let preferred_mode = modes.first().copied();
            let mut snapshot = OutputSnapshot::new(name.clone(), modes);
            snapshot.name = name;
            snapshot.current_mode = current_mode.or(preferred_mode);
            snapshot.preferred_mode = preferred_mode;
            outputs.push(snapshot.sanitized());
        }
        Ok(outputs)
    }
}

fn is_connector_name(name: &str) -> bool {
    let Some((card, connector)) = name.split_once('-') else {
        return false;
    };
    card.starts_with("card")
        && card[4..].chars().all(|character| character.is_ascii_digit())
        && !connector.is_empty()
        && !connector.starts_with("render")
        && !connector.starts_with("control")
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|contents| contents.trim().to_owned())
}

/// Parse one kernel/sysfs mode line such as `1920x1080` or `1920x1080@60`.
pub fn parse_mode(value: &str) -> Option<OutputMode> {
    let value = value.trim();
    let (dimensions, refresh) = value.split_once('@').unwrap_or((value, "60"));
    let (width, height) = dimensions.split_once('x')?;
    let width = width.parse::<i32>().ok()?;
    let height = height.parse::<i32>().ok()?;
    let refresh_mhz = parse_refresh_mhz(refresh)?;
    let mode = OutputMode::new(width, height, refresh_mhz);
    mode.is_valid().then_some(mode)
}

fn parse_modes(value: &str) -> Vec<OutputMode> {
    value.lines().filter_map(parse_mode).collect()
}

fn parse_refresh_mhz(value: &str) -> Option<i32> {
    let value = value.trim();
    if let Ok(integer) = value.parse::<i32>() {
        return (integer > 0).then_some(integer.saturating_mul(1_000));
    }
    let (whole, fraction) = value.split_once('.')?;
    let whole = whole.parse::<i32>().ok()?;
    let fraction = fraction.chars().take(3).collect::<String>();
    let fraction = fraction.parse::<i32>().ok()?;
    let scale = 10i32.pow(fraction.to_string().len() as u32);
    let mhz = whole.saturating_mul(1_000).saturating_add(
        fraction
            .saturating_mul(1_000)
            .checked_div(scale)
            .unwrap_or_default(),
    );
    (mhz > 0).then_some(mhz)
}

/// Diff two discovery snapshots into hotplug operations.
pub fn diff_snapshots(previous: &[OutputSnapshot], current: &[OutputSnapshot]) -> Vec<OutputHotplug> {
    let previous = previous
        .iter()
        .map(|snapshot| (snapshot.id.as_str(), snapshot))
        .collect::<BTreeMap<_, _>>();
    let current = current
        .iter()
        .map(|snapshot| (snapshot.id.as_str(), snapshot))
        .collect::<BTreeMap<_, _>>();
    let mut ids = BTreeSet::new();
    ids.extend(previous.keys().copied());
    ids.extend(current.keys().copied());

    ids.into_iter()
        .filter_map(|id| match (previous.get(id), current.get(id)) {
            (None, Some(snapshot)) => Some(OutputHotplug::Added((*snapshot).clone())),
            (Some(_), None) => Some(OutputHotplug::Removed(id.to_owned())),
            (Some(before), Some(after)) if *before != *after => {
                Some(OutputHotplug::Changed((*after).clone()))
            }
            _ => None,
        })
        .collect()
}

/// A connector-level hotplug result after a discovery rescan.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputHotplug {
    /// A connector was added.
    Added(OutputSnapshot),
    /// A connector changed mode/scale/metadata.
    Changed(OutputSnapshot),
    /// A connector disappeared.
    Removed(String),
}

/// A lightweight event emitted by the native udev source.
///
/// The event contains the DRM device path, not a guessed monitor record.  The
/// integrator must call [`SysfsOutputDiscovery::scan`] (or its DRM connector
/// scanner) and then [`diff_snapshots`] so connector removal is observed even
/// when the kernel sends events in a burst.
#[cfg(feature = "native-session")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeOutputEvent {
    /// A DRM device appeared.
    DeviceAdded {
        /// Kernel device number.
        device_id: u64,
        /// Device node path.
        path: PathBuf,
    },
    /// A connector or DRM device changed.
    DeviceChanged {
        /// Kernel device number.
        device_id: u64,
    },
    /// A DRM device disappeared.
    DeviceRemoved {
        /// Kernel device number.
        device_id: u64,
    },
}

#[cfg(feature = "native-session")]
fn map_udev_event(event: smithay::backend::udev::UdevEvent) -> NativeOutputEvent {
    match event {
        smithay::backend::udev::UdevEvent::Added { device_id, path } => NativeOutputEvent::DeviceAdded {
            device_id: device_id as u64,
            path,
        },
        smithay::backend::udev::UdevEvent::Changed { device_id } => NativeOutputEvent::DeviceChanged {
            device_id: device_id as u64,
        },
        smithay::backend::udev::UdevEvent::Removed { device_id } => NativeOutputEvent::DeviceRemoved {
            device_id: device_id as u64,
        },
    }
}

/// Smithay 0.7 udev source adapted to connector-rescan events.
#[cfg(feature = "native-session")]
#[derive(Debug)]
pub struct UdevOutputSource {
    backend: smithay::backend::udev::UdevBackend,
}

#[cfg(feature = "native-session")]
impl UdevOutputSource {
    /// Monitor DRM devices on one seat.
    pub fn new(seat: impl AsRef<str>) -> io::Result<Self> {
        Ok(Self {
            backend: smithay::backend::udev::UdevBackend::new(seat)?,
        })
    }

    /// Return initial GPU nodes before inserting the source in calloop.
    pub fn initial_devices(&self) -> Vec<(u64, PathBuf)> {
        self.backend
            .device_list()
            .map(|(device_id, path)| (device_id as u64, path.to_owned()))
            .collect()
    }

    /// Access the wrapped Smithay backend for diagnostics.
    pub fn backend(&self) -> &smithay::backend::udev::UdevBackend {
        &self.backend
    }
}

#[cfg(feature = "native-session")]
impl smithay::reexports::calloop::EventSource for UdevOutputSource {
    type Event = NativeOutputEvent;
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    fn process_events<F>(
        &mut self,
        readiness: smithay::reexports::calloop::Readiness,
        token: smithay::reexports::calloop::Token,
        mut callback: F,
    ) -> Result<smithay::reexports::calloop::PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        use smithay::reexports::calloop::EventSource;
        self.backend.process_events(readiness, token, |event, _| {
            callback(map_udev_event(event), &mut ());
        })
    }

    fn register(
        &mut self,
        poll: &mut smithay::reexports::calloop::Poll,
        factory: &mut smithay::reexports::calloop::TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        use smithay::reexports::calloop::EventSource;
        self.backend.register(poll, factory)
    }

    fn reregister(
        &mut self,
        poll: &mut smithay::reexports::calloop::Poll,
        factory: &mut smithay::reexports::calloop::TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        use smithay::reexports::calloop::EventSource;
        self.backend.reregister(poll, factory)
    }

    fn unregister(
        &mut self,
        poll: &mut smithay::reexports::calloop::Poll,
    ) -> smithay::reexports::calloop::Result<()> {
        use smithay::reexports::calloop::EventSource;
        self.backend.unregister(poll)
    }
}

/// Pure lifecycle events accepted from a libseat/logind notifier.
#[cfg(feature = "native-session")]
pub fn map_session_event(event: smithay::backend::session::Event) -> SessionEvent {
    match event {
        smithay::backend::session::Event::PauseSession => SessionEvent::Pause,
        smithay::backend::session::Event::ActivateSession => SessionEvent::Activate { vt: None },
    }
}

/// Smithay libseat session wrapper for native DRM/VT ownership.
#[cfg(feature = "native-session")]
#[derive(Debug)]
pub struct LibseatOutputSession {
    session: smithay::backend::session::libseat::LibSeatSession,
    notifier: smithay::backend::session::libseat::LibSeatSessionNotifier,
}

#[cfg(feature = "native-session")]
impl LibseatOutputSession {
    /// Open the configured libseat provider.
    pub fn new() -> Result<Self, String> {
        let (session, notifier) =
            smithay::backend::session::libseat::LibSeatSession::new().map_err(|error| error.to_string())?;
        Ok(Self { session, notifier })
    }

    /// Access the calloop notifier that reports VT pause/resume.
    pub fn notifier_mut(&mut self) -> &mut smithay::backend::session::libseat::LibSeatSessionNotifier {
        &mut self.notifier
    }

    /// Access the Smithay `Session` used by DRM and libinput.
    pub fn session(&self) -> &smithay::backend::session::libseat::LibSeatSession {
        &self.session
    }

    /// Request a validated VT switch through libseat.
    pub fn change_vt(&mut self, request: VtRequest) -> Result<(), String> {
        use smithay::backend::session::Session;
        let mut session = self.session.clone();
        session
            .change_vt(request.vt)
            .map_err(|error| format!("libseat VT switch failed: {error:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: i32, height: i32, refresh: i32) -> OutputMode {
        OutputMode::new(width, height, refresh)
    }

    fn output(id: &str, width: i32, height: i32) -> OutputSnapshot {
        OutputSnapshot::new(id, [mode(width, height, 60_000)])
    }

    #[test]
    fn requested_mode_wins_and_refresh_falls_to_nearest() {
        let modes = [
            mode(1920, 1080, 60_000),
            mode(1920, 1080, 120_000),
            mode(2560, 1440, 60_000),
        ];
        assert_eq!(
            select_mode(
                &modes,
                None,
                None,
                Some(ModeRequest {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: Some(100_000),
                })
            ),
            Some(mode(1920, 1080, 120_000))
        );
        assert_eq!(
            select_mode(&modes, Some(mode(2560, 1440, 60_000)), None, None),
            Some(mode(2560, 1440, 60_000))
        );
    }

    #[test]
    fn invalid_scale_and_mode_values_recover_to_safe_geometry() {
        let snapshot = OutputSnapshot {
            scale: f64::NAN,
            modes: vec![mode(1920, 1080, 60_000)],
            current_mode: Some(mode(1, 1, 1)),
            ..output("eDP-1", 1920, 1080)
        }
        .sanitized();
        assert_eq!(snapshot.scale, 1.0);
        assert_eq!(snapshot.selected_mode(), Some(mode(1920, 1080, 60_000)));
        assert_eq!(snapshot.logical_size(), (1920, 1080));
    }

    #[test]
    fn horizontal_layout_puts_primary_first_and_keeps_hidpi_logical_size() {
        let mut primary = output("HDMI-A-1", 1920, 1080);
        primary.primary = true;
        let mut secondary = output("eDP-1", 2560, 1600);
        secondary.scale = 2.0;
        let mut registry = OutputRegistry::new();
        registry.reconcile([secondary, primary], LayoutStrategy::Horizontal);
        assert_eq!(registry.focused_output(), Some("HDMI-A-1"));
        assert_eq!(registry.geometry("HDMI-A-1").unwrap().x, 0);
        assert_eq!(registry.geometry("eDP-1").unwrap().x, 1920);
        assert_eq!(registry.geometry("eDP-1").unwrap().width, 1280);
    }

    #[test]
    fn removed_focused_output_recovers_to_primary() {
        let mut primary = output("eDP-1", 1920, 1080);
        primary.primary = true;
        let mut registry = OutputRegistry::new();
        registry.reconcile(
            [primary.clone(), output("HDMI-A-1", 1280, 720)],
            LayoutStrategy::Horizontal,
        );
        registry.focus_output("HDMI-A-1").unwrap();
        let changes = registry.reconcile([primary], LayoutStrategy::Horizontal);
        assert!(changes.contains(&OutputChange::Removed("HDMI-A-1".into())));
        assert!(changes.contains(&OutputChange::FocusChanged(Some("eDP-1".into()))));
        assert_eq!(registry.focused_output(), Some("eDP-1"));
    }

    #[test]
    fn directional_focus_uses_geometry_not_connector_sorting() {
        let mut left = output("left", 100, 100);
        left.position = Some(LogicalPoint { x: 0, y: 0 });
        let mut right = output("right", 100, 100);
        right.position = Some(LogicalPoint { x: 100, y: 0 });
        let mut registry = OutputRegistry::new();
        registry.reconcile([left, right], LayoutStrategy::Preserve);
        registry.focus_output("left").unwrap();
        registry.focus_direction(OutputDirection::Right);
        assert_eq!(registry.focused_output(), Some("right"));
    }

    #[test]
    fn session_pause_clears_focus_and_activation_restores_it() {
        let mut registry = OutputRegistry::new();
        registry.reconcile([output("eDP-1", 1920, 1080)], LayoutStrategy::Horizontal);
        assert_eq!(registry.focused_output(), Some("eDP-1"));
        registry.session_event(SessionEvent::Pause);
        assert_eq!(registry.focused_output(), None);
        registry.session_event(SessionEvent::Activate { vt: Some(2) });
        assert_eq!(registry.focused_output(), Some("eDP-1"));
        assert_eq!(registry.session_state(), SessionState::Active { vt: Some(2) });
    }

    #[test]
    fn snapshot_diff_is_idempotent_and_reports_hotplug() {
        let before = [output("eDP-1", 1920, 1080)];
        let mut changed = output("eDP-1", 2560, 1600);
        changed.scale = 2.0;
        let after = [changed, output("HDMI-A-1", 1920, 1080)];
        let events = diff_snapshots(&before, &after);
        assert!(matches!(events[0], OutputHotplug::Changed(_)));
        assert!(matches!(events[1], OutputHotplug::Added(_)));
        assert!(diff_snapshots(&after, &after).is_empty());
    }

    #[test]
    fn parses_sysfs_modes_and_refreshes() {
        assert_eq!(parse_mode("1920x1080"), Some(mode(1920, 1080, 60_000)));
        assert_eq!(parse_mode("1920x1080@59.94"), Some(mode(1920, 1080, 59_940)));
        assert_eq!(parse_mode("garbage"), None);
    }

    #[test]
    fn validates_vt_range_and_session_state() {
        let registry = OutputRegistry::new();
        assert_eq!(registry.request_vt(0), Err(OutputError::InvalidVt(0)));
        assert_eq!(registry.request_vt(2), Ok(VtRequest { vt: 2 }));
        let mut paused = registry;
        paused.session_event(SessionEvent::Pause);
        assert_eq!(paused.request_vt(2), Err(OutputError::SessionInactive));
    }
}
