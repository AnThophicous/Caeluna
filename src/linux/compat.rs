//! Low-cost compatibility boundaries for a native Rouch Wayland session.
//!
//! This module deliberately has no renderer, Smithay, UI, or D-Bus crate
//! dependency.  It owns the parts that can be made safe and deterministic at
//! this boundary: locating XWayland, constructing an argv/env launch, keeping
//! its process lifecycle, describing the real Wayland data-device contracts,
//! and reporting conservative portal/D-Bus capability states.
//!
//! A `Confirmed` observation must come from the protocol/D-Bus layer that owns
//! the connection.  File presence, environment variables, and executable
//! discovery only produce `Degraded` evidence.  That distinction prevents a
//! missing portal backend or an unbound XWayland bridge from being presented
//! as a working feature.

use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fmt, fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// A capability state that can be shown to a caller without implying more
/// integration than has actually been observed.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CapabilityState {
    /// The owning protocol/service was queried and the required operation was
    /// confirmed.
    Available,
    /// Some prerequisite is present, but the operation or bridge was not
    /// confirmed.  Callers should use a bounded fallback.
    Degraded,
    /// A required prerequisite is absent or a confirmed probe failed.
    Unavailable,
}

impl CapabilityState {
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    pub const fn is_degraded(self) -> bool {
        matches!(self, Self::Degraded)
    }

    pub const fn is_unavailable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

/// Evidence used by a probe.  `Detected` means that a cheap local discovery
/// found a prerequisite; it is intentionally not the same as a successful
/// protocol call.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProbeEvidence {
    /// A real protocol or D-Bus query completed successfully.
    Confirmed,
    /// A binary, environment entry, or activatable service was discovered.
    Detected,
    /// The local probe could not find the prerequisite.
    #[default]
    NotDetected,
    /// A real probe ran and reported an error.
    Failed,
}

impl ProbeEvidence {
    const fn state(self) -> CapabilityState {
        match self {
            Self::Confirmed => CapabilityState::Available,
            Self::Detected => CapabilityState::Degraded,
            Self::NotDetected | Self::Failed => CapabilityState::Unavailable,
        }
    }
}

/// User-facing capability identifiers used by the compatibility matrix.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    XWayland,
    X11Applications,
    DataDevice,
    Clipboard,
    DragAndDrop,
    PrimarySelection,
    DbusSession,
    Portals,
    PortalClipboard,
    Screencast,
    Notifications,
}

impl Capability {
    pub const ALL: [Self; 11] = [
        Self::XWayland,
        Self::X11Applications,
        Self::DataDevice,
        Self::Clipboard,
        Self::DragAndDrop,
        Self::PrimarySelection,
        Self::DbusSession,
        Self::Portals,
        Self::PortalClipboard,
        Self::Screencast,
        Self::Notifications,
    ];
}

/// One matrix entry.  `detail` is intentionally plain text so a UI or log
/// sink can display the reason without importing any presentation code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityStatus {
    pub capability: Capability,
    pub state: CapabilityState,
    pub evidence: ProbeEvidence,
    pub detail: String,
}

impl CapabilityStatus {
    fn new(capability: Capability, evidence: ProbeEvidence, detail: impl Into<String>) -> Self {
        Self {
            capability,
            state: evidence.state(),
            evidence,
            detail: detail.into(),
        }
    }

    fn with_state(
        capability: Capability,
        state: CapabilityState,
        evidence: ProbeEvidence,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            capability,
            state,
            evidence,
            detail: detail.into(),
        }
    }
}

/// A snapshot of compatibility discovered once at startup or after an
/// explicit session restart.  It does not poll, render, or spawn anything.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilityMatrix {
    statuses: BTreeMap<Capability, CapabilityStatus>,
}

impl CapabilityMatrix {
    /// Build a conservative snapshot from the XWayland, Wayland, and portal
    /// observations supplied by their owning adapters.
    pub fn from_probes(xwayland: &XWaylandDetection, wayland: WaylandProbe, portals: PortalProbe) -> Self {
        let mut statuses = BTreeMap::new();

        let xwayland_evidence = match xwayland.state {
            CapabilityState::Available => ProbeEvidence::Detected,
            CapabilityState::Degraded => ProbeEvidence::Detected,
            CapabilityState::Unavailable => ProbeEvidence::NotDetected,
        };
        let (x11_state, x11_evidence, x11_detail) = if xwayland.state.is_unavailable() {
            (
                CapabilityState::Unavailable,
                ProbeEvidence::NotDetected,
                format!("XWayland fallback unavailable: {}", xwayland.reason),
            )
        } else {
            match wayland.xwayland_bridge {
                ProbeEvidence::Confirmed => (
                    CapabilityState::Available,
                    ProbeEvidence::Confirmed,
                    "XWayland executable and compositor bridge are confirmed.".to_owned(),
                ),
                ProbeEvidence::Failed => (
                    CapabilityState::Unavailable,
                    ProbeEvidence::Failed,
                    "XWayland exists, but its compositor bridge probe failed.".to_owned(),
                ),
                ProbeEvidence::Detected | ProbeEvidence::NotDetected => (
                    CapabilityState::Degraded,
                    ProbeEvidence::Detected,
                    "XWayland can be started, but the compositor bridge is not confirmed; use native Wayland first.".to_owned(),
                ),
            }
        };
        statuses.insert(
            Capability::XWayland,
            CapabilityStatus::with_state(
                Capability::XWayland,
                xwayland.state,
                xwayland_evidence,
                xwayland.reason.clone(),
            ),
        );
        statuses.insert(
            Capability::X11Applications,
            CapabilityStatus::with_state(Capability::X11Applications, x11_state, x11_evidence, x11_detail),
        );

        let data_device = status_from_evidence(
            Capability::DataDevice,
            wayland.data_device_manager,
            "wl_data_device_manager is confirmed by the compositor protocol layer.",
            "wl_data_device_manager was discovered but its runtime binding is not confirmed.",
            "wl_data_device_manager was not confirmed; clipboard and DnD cannot be assumed.",
        );
        statuses.insert(Capability::DataDevice, data_device.clone());

        let clipboard = if wayland.data_device_manager == ProbeEvidence::Confirmed {
            CapabilityStatus::new(
                Capability::Clipboard,
                ProbeEvidence::Confirmed,
                "Clipboard uses the real wl_data_device selection/offer transfer path.",
            )
        } else if portals.clipboard == ProbeEvidence::Confirmed {
            CapabilityStatus::with_state(
                Capability::Clipboard,
                CapabilityState::Degraded,
                ProbeEvidence::Confirmed,
                "Portal clipboard is confirmed, but it is session-scoped and does not replace wl_data_device for ordinary Wayland clients.",
            )
        } else if data_device.state == CapabilityState::Degraded {
            CapabilityStatus::with_state(
                Capability::Clipboard,
                CapabilityState::Degraded,
                ProbeEvidence::Detected,
                "A data-device prerequisite exists, but clipboard ownership/transfer is not confirmed.",
            )
        } else {
            CapabilityStatus::new(
                Capability::Clipboard,
                ProbeEvidence::NotDetected,
                "No confirmed wl_data_device or session-scoped portal clipboard path.",
            )
        };
        statuses.insert(Capability::Clipboard, clipboard);

        let drag_and_drop = if wayland.drag_and_drop == ProbeEvidence::Confirmed {
            CapabilityStatus::new(
                Capability::DragAndDrop,
                ProbeEvidence::Confirmed,
                "Core wl_data_device drag-and-drop enter/motion/drop/leave handling is confirmed.",
            )
        } else if wayland.data_device_manager == ProbeEvidence::Detected
            || wayland.drag_and_drop == ProbeEvidence::Detected
        {
            CapabilityStatus::with_state(
                Capability::DragAndDrop,
                CapabilityState::Degraded,
                ProbeEvidence::Detected,
                "The data-device manager exists, but drag-and-drop input handling is not confirmed.",
            )
        } else {
            CapabilityStatus::new(
                Capability::DragAndDrop,
                ProbeEvidence::NotDetected,
                "No confirmed wl_data_device drag-and-drop transport.",
            )
        };
        statuses.insert(Capability::DragAndDrop, drag_and_drop);
        statuses.insert(
            Capability::PrimarySelection,
            status_from_evidence(
                Capability::PrimarySelection,
                wayland.primary_selection,
                "zwp_primary_selection_v1 is confirmed.",
                "Primary selection was detected, but the optional protocol is not confirmed.",
                "zwp_primary_selection_v1 is optional and was not confirmed.",
            ),
        );

        statuses.insert(
            Capability::DbusSession,
            status_from_evidence(
                Capability::DbusSession,
                portals.session_bus,
                "A live session D-Bus connection was confirmed by the D-Bus adapter.",
                "DBUS_SESSION_BUS_ADDRESS is present; connection/reachability is not confirmed here.",
                "No session D-Bus address or successful connection was observed.",
            ),
        );

        statuses.insert(
            Capability::Portals,
            portal_status(
                Capability::Portals,
                portals.desktop,
                portals.session_bus,
                "org.freedesktop.portal.Desktop is confirmed on the session bus.",
                "The portal service is discoverable, but a live D-Bus name/interface was not confirmed.",
                "No confirmed xdg-desktop-portal service is available.",
            ),
        );
        statuses.insert(
            Capability::PortalClipboard,
            optional_portal_status(
                Capability::PortalClipboard,
                portals.clipboard,
                portals.desktop,
                "org.freedesktop.portal.Clipboard is confirmed for a compatible portal session.",
                "Clipboard portal support is not confirmed; it is session-scoped and may require Remote Desktop or Input Capture.",
            ),
        );
        statuses.insert(
            Capability::Screencast,
            optional_portal_status(
                Capability::Screencast,
                portals.screencast,
                portals.desktop,
                "ScreenCast CreateSession/SelectSources/Start/OpenPipeWireRemote is confirmed.",
                "ScreenCast was not confirmed; no PipeWire stream or user consent is assumed.",
            ),
        );
        statuses.insert(
            Capability::Notifications,
            optional_portal_status(
                Capability::Notifications,
                portals.notifications,
                portals.session_bus,
                "org.freedesktop.Notifications is confirmed on the session bus.",
                "A notification service may be activatable, but ownership and Notify support are not confirmed.",
            ),
        );

        Self { statuses }
    }

    /// Probe only cheap local evidence.  Wayland globals and live D-Bus
    /// interfaces remain `NotDetected` until their owning connection reports
    /// them, so this function never performs I/O in the render loop.
    pub fn detect() -> Self {
        let environment = HostEnvironment::from_process();
        let xwayland = XWaylandAdapter::from_environment(&environment);
        let portals = PortalProbe::from_environment(&environment);
        Self::from_probes(&xwayland.detection, WaylandProbe::default(), portals)
    }

    pub fn status(&self, capability: Capability) -> Option<&CapabilityStatus> {
        self.statuses.get(&capability)
    }

    pub fn state(&self, capability: Capability) -> CapabilityState {
        self.status(capability)
            .map(|status| status.state)
            .unwrap_or(CapabilityState::Unavailable)
    }

    pub fn iter(&self) -> impl Iterator<Item = &CapabilityStatus> {
        self.statuses.values()
    }
}

fn status_from_evidence(
    capability: Capability,
    evidence: ProbeEvidence,
    confirmed_detail: &str,
    detected_detail: &str,
    missing_detail: &str,
) -> CapabilityStatus {
    let detail = match evidence {
        ProbeEvidence::Confirmed => confirmed_detail,
        ProbeEvidence::Detected => detected_detail,
        ProbeEvidence::NotDetected | ProbeEvidence::Failed => missing_detail,
    };
    CapabilityStatus::new(capability, evidence, detail)
}

fn portal_status(
    capability: Capability,
    portal: ProbeEvidence,
    bus: ProbeEvidence,
    confirmed_detail: &str,
    detected_detail: &str,
    missing_detail: &str,
) -> CapabilityStatus {
    match (portal, bus) {
        (ProbeEvidence::Confirmed, ProbeEvidence::Confirmed) => {
            CapabilityStatus::new(capability, ProbeEvidence::Confirmed, confirmed_detail)
        }
        (ProbeEvidence::Failed, _) | (_, ProbeEvidence::Failed) => CapabilityStatus::with_state(
            capability,
            CapabilityState::Unavailable,
            ProbeEvidence::Failed,
            missing_detail,
        ),
        (ProbeEvidence::NotDetected, ProbeEvidence::NotDetected) => {
            CapabilityStatus::new(capability, ProbeEvidence::NotDetected, missing_detail)
        }
        _ => CapabilityStatus::with_state(
            capability,
            CapabilityState::Degraded,
            ProbeEvidence::Detected,
            detected_detail,
        ),
    }
}

fn optional_portal_status(
    capability: Capability,
    interface: ProbeEvidence,
    desktop: ProbeEvidence,
    confirmed_detail: &str,
    missing_detail: &str,
) -> CapabilityStatus {
    match interface {
        ProbeEvidence::Confirmed => {
            CapabilityStatus::new(capability, ProbeEvidence::Confirmed, confirmed_detail)
        }
        ProbeEvidence::Failed => CapabilityStatus::with_state(
            capability,
            CapabilityState::Unavailable,
            ProbeEvidence::Failed,
            missing_detail,
        ),
        ProbeEvidence::Detected => CapabilityStatus::with_state(
            capability,
            CapabilityState::Degraded,
            ProbeEvidence::Detected,
            missing_detail,
        ),
        ProbeEvidence::NotDetected if desktop != ProbeEvidence::NotDetected => CapabilityStatus::with_state(
            capability,
            CapabilityState::Degraded,
            ProbeEvidence::Detected,
            missing_detail,
        ),
        ProbeEvidence::NotDetected => {
            CapabilityStatus::new(capability, ProbeEvidence::NotDetected, missing_detail)
        }
    }
}

/// A snapshot of process environment relevant to a native Wayland session.
/// Keeping it injectable makes detection deterministic in tests and avoids
/// global environment mutation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostEnvironment {
    pub path: Option<OsString>,
    pub xwayland_executable: Option<PathBuf>,
    pub wayland_display: Option<OsString>,
    pub runtime_dir: Option<PathBuf>,
    pub dbus_session_bus_address: Option<OsString>,
    pub data_home: Option<PathBuf>,
    pub data_dirs: Vec<PathBuf>,
}

impl HostEnvironment {
    pub fn from_process() -> Self {
        let home = env::var_os("HOME").map(PathBuf::from);
        let data_home = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".local/share")));
        let data_dirs = env::var_os("XDG_DATA_DIRS")
            .map(|value| env::split_paths(&value).collect())
            .unwrap_or_else(|| vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]);

        Self {
            path: env::var_os("PATH"),
            xwayland_executable: env::var_os("ROUCH_XWAYLAND").map(PathBuf::from),
            wayland_display: env::var_os("WAYLAND_DISPLAY"),
            runtime_dir: env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            dbus_session_bus_address: env::var_os("DBUS_SESSION_BUS_ADDRESS"),
            data_home,
            data_dirs,
        }
    }

    fn service_file(&self, file_name: &str) -> Option<PathBuf> {
        let mut roots = Vec::with_capacity(1 + self.data_dirs.len());
        if let Some(data_home) = &self.data_home {
            roots.push(data_home.clone());
        }
        roots.extend(self.data_dirs.iter().cloned());
        roots
            .into_iter()
            .map(|root| root.join("dbus-1/services").join(file_name))
            .find(|path| path.is_file())
    }
}

/// Cheap discovery of Wayland protocol/bridge state.  The compositor should
/// replace `NotDetected` with `Confirmed` only after its real protocol layer
/// has installed the corresponding handler.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WaylandProbe {
    pub data_device_manager: ProbeEvidence,
    pub drag_and_drop: ProbeEvidence,
    pub primary_selection: ProbeEvidence,
    pub xwayland_bridge: ProbeEvidence,
}

/// D-Bus/portal observations supplied by a real session-bus adapter, or by the
/// cheap environment probe below.  A boolean is deliberately not used here:
/// `Detected` and `Confirmed` have different user-visible consequences.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PortalProbe {
    pub session_bus: ProbeEvidence,
    pub desktop: ProbeEvidence,
    pub clipboard: ProbeEvidence,
    pub screencast: ProbeEvidence,
    pub notifications: ProbeEvidence,
}

impl PortalProbe {
    /// Find only session-bus addresses and activatable service files.  This
    /// performs bounded metadata checks once; it never invokes `dbus-send`,
    /// `busctl`, a shell, or a portal request on behalf of the caller.
    pub fn from_environment(environment: &HostEnvironment) -> Self {
        let session_bus = if non_empty_os_string(environment.dbus_session_bus_address.as_deref()) {
            ProbeEvidence::Detected
        } else {
            ProbeEvidence::NotDetected
        };
        let desktop = if environment
            .service_file("org.freedesktop.portal.Desktop.service")
            .is_some()
        {
            ProbeEvidence::Detected
        } else {
            ProbeEvidence::NotDetected
        };
        let notifications = if environment
            .service_file("org.freedesktop.Notifications.service")
            .is_some()
        {
            ProbeEvidence::Detected
        } else {
            ProbeEvidence::NotDetected
        };

        Self {
            session_bus,
            desktop,
            clipboard: ProbeEvidence::NotDetected,
            screencast: ProbeEvidence::NotDetected,
            notifications,
        }
    }
}

fn non_empty_os_string(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// The result of locating XWayland without launching it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XWaylandDetection {
    pub state: CapabilityState,
    pub executable: Option<PathBuf>,
    pub wayland_display: Option<OsString>,
    pub runtime_dir: Option<PathBuf>,
    pub reason: String,
}

impl XWaylandDetection {
    pub fn can_spawn(&self) -> bool {
        self.state == CapabilityState::Available
            && self.executable.is_some()
            && self.wayland_display.is_some()
            && self.runtime_dir.is_some()
    }
}

/// XWayland process adapter.  It does not claim that an X11 client can map
/// until the compositor reports `WaylandProbe::xwayland_bridge = Confirmed`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XWaylandAdapter {
    pub detection: XWaylandDetection,
}

impl XWaylandAdapter {
    pub fn from_environment(environment: &HostEnvironment) -> Self {
        let executable = environment
            .xwayland_executable
            .as_deref()
            .filter(|path| is_executable_file(path))
            .and_then(canonical_path)
            .or_else(|| find_executable("Xwayland", environment.path.as_deref()));
        let wayland_display = environment
            .wayland_display
            .clone()
            .filter(|display| valid_environment_value(display));
        let runtime_dir = environment
            .runtime_dir
            .clone()
            .filter(|path| path.is_absolute() && path.is_dir());

        let (state, reason) = match (&executable, &wayland_display, &runtime_dir) {
            (None, _, _) => (
                CapabilityState::Unavailable,
                "Xwayland was not found in PATH or at ROUCH_XWAYLAND; native Wayland remains the fallback.".to_owned(),
            ),
            (Some(_), None, _) => (
                CapabilityState::Degraded,
                "Xwayland was found, but WAYLAND_DISPLAY is missing or invalid; native Wayland remains the fallback.".to_owned(),
            ),
            (Some(_), Some(_), None) => (
                CapabilityState::Degraded,
                "Xwayland was found, but XDG_RUNTIME_DIR is missing or is not a directory; native Wayland remains the fallback.".to_owned(),
            ),
            (Some(path), Some(display), Some(runtime)) => (
                CapabilityState::Available,
                format!(
                    "Xwayland can be launched at {} against WAYLAND_DISPLAY={} and XDG_RUNTIME_DIR={}; compositor bridge still requires confirmation.",
                    path.display(),
                    display.to_string_lossy(),
                    runtime.display()
                ),
            ),
        };

        Self {
            detection: XWaylandDetection {
                state,
                executable,
                wayland_display,
                runtime_dir,
                reason,
            },
        }
    }

    pub fn detection(&self) -> &XWaylandDetection {
        &self.detection
    }

    pub fn launch_config(&self) -> Result<XWaylandLaunchConfig, XWaylandError> {
        if !self.detection.can_spawn() {
            return Err(XWaylandError::NotReady(self.detection.reason.clone()));
        }
        XWaylandLaunchConfig::new(
            self.detection.executable.clone().expect("can_spawn checked"),
            self.detection.wayland_display.clone().expect("can_spawn checked"),
            self.detection.runtime_dir.clone().expect("can_spawn checked"),
        )
    }

    /// Start XWayland when all local prerequisites are present, otherwise
    /// return an explicit native-Wayland fallback instead of pretending X11
    /// support is active.
    pub fn start_or_fallback(&self) -> XWaylandStart {
        let config = match self.launch_config() {
            Ok(config) => config,
            Err(error) => {
                return XWaylandStart::Fallback {
                    reason: error.to_string(),
                };
            }
        };
        match XWaylandProcess::spawn(&config) {
            Ok(process) => XWaylandStart::Running(process),
            Err(error) => XWaylandStart::Fallback {
                reason: format!("XWayland launch failed: {error}; native Wayland remains active."),
            },
        }
    }
}

/// Exact, fixed XWayland launch configuration.  There is no public arbitrary
/// argv/environment escape hatch, which keeps the process boundary free of
/// shell interpretation and accidental option injection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XWaylandLaunchConfig {
    pub executable: PathBuf,
    pub wayland_display: OsString,
    pub runtime_dir: PathBuf,
}

impl XWaylandLaunchConfig {
    pub fn new(
        executable: PathBuf,
        wayland_display: OsString,
        runtime_dir: PathBuf,
    ) -> Result<Self, XWaylandError> {
        if !executable.is_absolute() || !is_executable_file(&executable) {
            return Err(XWaylandError::InvalidConfig(
                "XWayland executable must be an existing absolute executable file",
            ));
        }
        if !valid_environment_value(&wayland_display) {
            return Err(XWaylandError::InvalidConfig(
                "WAYLAND_DISPLAY must be a non-empty value without control characters",
            ));
        }
        if !runtime_dir.is_absolute() || !runtime_dir.is_dir() {
            return Err(XWaylandError::InvalidConfig(
                "XDG_RUNTIME_DIR must be an existing absolute directory",
            ));
        }
        Ok(Self {
            executable,
            wayland_display,
            runtime_dir,
        })
    }

    /// The actual XWayland rootless launch contract.  `-displayfd 1` sends the
    /// allocated X display number to a pipe owned by `XWaylandProcess`.
    pub fn command_spec(&self) -> XWaylandCommandSpec {
        XWaylandCommandSpec {
            program: self.executable.clone(),
            args: vec![
                OsString::from("-rootless"),
                OsString::from("-terminate"),
                OsString::from("-displayfd"),
                OsString::from("1"),
            ],
            environment: vec![
                (OsString::from("WAYLAND_DISPLAY"), self.wayland_display.clone()),
                (
                    OsString::from("XDG_RUNTIME_DIR"),
                    self.runtime_dir.as_os_str().to_owned(),
                ),
                (OsString::from("XDG_SESSION_TYPE"), OsString::from("wayland")),
            ],
        }
    }
}

/// A testable representation of the fixed process boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XWaylandCommandSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// These values override the inherited environment.  The inherited
    /// environment is not interpreted by a shell.
    pub environment: Vec<(OsString, OsString)>,
}

/// The only two outcomes a caller should branch on after requesting XWayland.
pub enum XWaylandStart {
    Running(XWaylandProcess),
    Fallback { reason: String },
}

impl fmt::Debug for XWaylandStart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running(process) => formatter
                .debug_tuple("Running")
                .field(&process.lifecycle_snapshot())
                .finish(),
            Self::Fallback { reason } => formatter
                .debug_struct("Fallback")
                .field("reason", reason)
                .finish(),
        }
    }
}

/// A process exit without exposing platform-specific `ExitStatus` internals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XWaylandExit {
    pub code: Option<i32>,
    pub success: bool,
}

/// Observable lifecycle of the supervised XWayland child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XWaylandLifecycle {
    Running,
    Exited(XWaylandExit),
}

/// A supervised XWayland process.  stdout is drained by one blocking reader
/// thread so `-displayfd` cannot deadlock the child; no polling loop is used.
pub struct XWaylandProcess {
    child: Child,
    display_number: Arc<Mutex<Option<u32>>>,
    display_reader: Option<JoinHandle<()>>,
    exit: Option<XWaylandExit>,
}

impl fmt::Debug for XWaylandProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XWaylandProcess")
            .field("display_number", &self.display_number())
            .field("lifecycle", &self.lifecycle_snapshot())
            .finish_non_exhaustive()
    }
}

impl XWaylandProcess {
    pub fn spawn(config: &XWaylandLaunchConfig) -> Result<Self, XWaylandError> {
        let spec = config.command_spec();
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in &spec.environment {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(XWaylandError::Spawn)?;
        let stdout = child.stdout.take().ok_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            XWaylandError::MissingDisplayPipe
        })?;
        let display_number = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&display_number);
        let display_reader = thread::Builder::new()
            .name("rouch-xwayland-display".to_owned())
            .spawn(move || drain_display_fd(stdout, sink))
            .map_err(|error| {
                let _ = child.kill();
                let _ = child.wait();
                XWaylandError::Reader(error)
            })?;

        Ok(Self {
            child,
            display_number,
            display_reader: Some(display_reader),
            exit: None,
        })
    }

    pub fn display_number(&self) -> Option<u32> {
        self.display_number.lock().ok().and_then(|number| *number)
    }

    pub fn display_name(&self) -> Option<String> {
        self.display_number().map(|number| format!(":{number}"))
    }

    pub fn lifecycle(&mut self) -> Result<XWaylandLifecycle, XWaylandError> {
        if let Some(exit) = self.exit {
            return Ok(XWaylandLifecycle::Exited(exit));
        }
        match self.child.try_wait().map_err(XWaylandError::Lifecycle)? {
            Some(status) => {
                let exit = exit_from_status(status);
                self.exit = Some(exit);
                Ok(XWaylandLifecycle::Exited(exit))
            }
            None => Ok(XWaylandLifecycle::Running),
        }
    }

    pub fn lifecycle_snapshot(&self) -> XWaylandLifecycle {
        self.exit
            .map(XWaylandLifecycle::Exited)
            .unwrap_or(XWaylandLifecycle::Running)
    }

    /// Wait briefly for XWayland's `-displayfd` line without a busy loop.
    pub fn wait_for_display(&mut self, timeout: Duration) -> Result<Option<String>, XWaylandError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(display) = self.display_name() {
                return Ok(Some(display));
            }
            if !matches!(self.lifecycle()?, XWaylandLifecycle::Running) || Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(4));
        }
    }

    /// Explicitly terminate the child and drain its display pipe.  Drop also
    /// performs this cleanup, so a failed compositor setup cannot orphan it.
    pub fn shutdown(&mut self) -> Result<XWaylandExit, XWaylandError> {
        if self.exit.is_none() {
            if self.child.try_wait().map_err(XWaylandError::Lifecycle)?.is_none() {
                self.child.kill().map_err(XWaylandError::Shutdown)?;
            }
            let status = self.child.wait().map_err(XWaylandError::Shutdown)?;
            self.exit = Some(exit_from_status(status));
        }
        if let Some(reader) = self.display_reader.take() {
            let _ = reader.join();
        }
        Ok(self.exit.expect("shutdown records an exit status"))
    }
}

impl Drop for XWaylandProcess {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn drain_display_fd(stdout: impl io::Read, sink: Arc<Mutex<Option<u32>>>) {
    let reader = BufReader::new(stdout);
    for line in reader.lines().map_while(Result::ok) {
        let Ok(number) = line.trim().parse::<u32>() else {
            continue;
        };
        if number > 65_535 {
            continue;
        }
        if let Ok(mut current) = sink.lock() {
            if current.is_none() {
                *current = Some(number);
            }
        }
    }
}

fn exit_from_status(status: ExitStatus) -> XWaylandExit {
    XWaylandExit {
        code: status.code(),
        success: status.success(),
    }
}

/// Locate an executable from an explicit absolute override or PATH entries.
/// Empty PATH entries are skipped rather than interpreted as the current
/// directory, avoiding accidental execution of a working-directory binary.
pub fn find_executable(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return None;
    }
    let path = path?;
    env::split_paths(path)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(name))
        .filter(|candidate| is_executable_file(candidate))
        .filter_map(|candidate| canonical_path(&candidate))
        .next()
}

fn canonical_path(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path)
        .ok()
        .filter(|path| is_executable_file(path))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn valid_environment_value(value: &OsStr) -> bool {
    let Some(value) = value.to_str() else {
        return false;
    };
    !value.is_empty() && !value.chars().any(char::is_control)
}

/// Protocol names used by the real core Wayland data-device API.
pub mod wayland_data_device {
    pub const MANAGER_INTERFACE: &str = "wl_data_device_manager";
    pub const DEVICE_INTERFACE: &str = "wl_data_device";
    pub const SOURCE_INTERFACE: &str = "wl_data_source";
    pub const OFFER_INTERFACE: &str = "wl_data_offer";
    pub const PRIMARY_SELECTION_INTERFACE: &str = "zwp_primary_selection_v1";
    pub const PORTAL_FILE_TRANSFER_MIME: &str = "application/vnd.portal.filetransfer";
}

/// Which Wayland data-device transfer is being described.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DataTransferKind {
    Clipboard,
    PrimarySelection,
    DragAndDrop,
}

/// A bounded, protocol-neutral data offer.  The compositor-specific adapter
/// maps this to `wl_data_source`/`wl_data_offer` messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataOfferContract {
    pub kind: DataTransferKind,
    pub mime_types: Vec<String>,
    pub serial: Option<u32>,
}

impl DataOfferContract {
    pub const MAX_MIME_TYPES: usize = 64;
    pub const MAX_MIME_LENGTH: usize = 255;

    pub fn new<I, S>(
        kind: DataTransferKind,
        mime_types: I,
        serial: Option<u32>,
    ) -> Result<Self, DataDeviceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut normalized = Vec::new();
        for mime_type in mime_types {
            let mime_type = mime_type.into();
            validate_mime_type(&mime_type)?;
            if !normalized.iter().any(|known| known == &mime_type) {
                if normalized.len() == Self::MAX_MIME_TYPES {
                    return Err(DataDeviceError::TooManyMimeTypes);
                }
                normalized.push(mime_type);
            }
        }
        if normalized.is_empty() {
            return Err(DataDeviceError::NoMimeTypes);
        }
        Ok(Self {
            kind,
            mime_types: normalized,
            serial,
        })
    }

    pub fn accepts(&self, mime_type: &str) -> bool {
        self.mime_types.iter().any(|known| known == mime_type)
    }
}

/// Operations that a real `wl_data_device` adapter must implement.  The
/// contract carries no bytes and performs no fake success by itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataDeviceRequest {
    ReadClipboard { mime_type: String },
    WriteClipboard { mime_type: String, serial: u32 },
    AcceptDrop { serial: u32, mime_type: Option<String> },
    FinishDrop { success: bool },
    Cancel,
}

impl DataDeviceRequest {
    pub fn read_clipboard(mime_type: &str) -> Result<Self, DataDeviceError> {
        validate_mime_type(mime_type)?;
        Ok(Self::ReadClipboard {
            mime_type: mime_type.to_owned(),
        })
    }

    pub fn write_clipboard(mime_type: &str, serial: u32) -> Result<Self, DataDeviceError> {
        validate_mime_type(mime_type)?;
        Ok(Self::WriteClipboard {
            mime_type: mime_type.to_owned(),
            serial,
        })
    }

    pub fn accept_drop(serial: u32, mime_type: Option<&str>) -> Result<Self, DataDeviceError> {
        if let Some(mime_type) = mime_type {
            validate_mime_type(mime_type)?;
        }
        Ok(Self::AcceptDrop {
            serial,
            mime_type: mime_type.map(str::to_owned),
        })
    }
}

/// A capability guard used by a concrete Smithay/Wayland transport before it
/// sends a request.  It is intentionally cheap and does not hold a socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataDeviceContract {
    pub data_device: CapabilityState,
    pub clipboard: CapabilityState,
    pub primary_selection: CapabilityState,
    pub drag_and_drop: CapabilityState,
}

impl DataDeviceContract {
    pub const fn from_probe(probe: WaylandProbe) -> Self {
        Self {
            data_device: probe.data_device_manager.state(),
            clipboard: probe.data_device_manager.state(),
            primary_selection: probe.primary_selection.state(),
            drag_and_drop: probe.drag_and_drop.state(),
        }
    }

    pub fn require_clipboard(&self) -> Result<(), DataDeviceError> {
        require_capability(self.clipboard, "clipboard")
    }

    pub fn require_drag_and_drop(&self) -> Result<(), DataDeviceError> {
        require_capability(self.drag_and_drop, "drag-and-drop")
    }

    pub fn require_primary_selection(&self) -> Result<(), DataDeviceError> {
        require_capability(self.primary_selection, "primary selection")
    }
}

/// The integration boundary for clipboard/data-device implementations.  A
/// concrete transport must map these methods to real Wayland requests and
/// return an error when the corresponding contract is not confirmed.
pub trait DataDeviceTransport {
    fn request(&mut self, request: DataDeviceRequest) -> Result<(), DataDeviceError>;
}

/// The integration boundary for DnD input.  Coordinates are logical surface
/// coordinates; the compositor remains responsible for serial validation and
/// focus/grab lifetime.
pub trait DragAndDropTransport {
    fn enter(&mut self, offer: DataOfferContract, x: f64, y: f64) -> Result<(), DataDeviceError>;
    fn motion(&mut self, serial: u32, x: f64, y: f64) -> Result<(), DataDeviceError>;
    fn drop(&mut self, serial: u32) -> Result<(), DataDeviceError>;
    fn leave(&mut self) -> Result<(), DataDeviceError>;
}

fn require_capability(state: CapabilityState, capability: &'static str) -> Result<(), DataDeviceError> {
    if state == CapabilityState::Available {
        Ok(())
    } else {
        Err(DataDeviceError::CapabilityNotConfirmed { capability, state })
    }
}

fn validate_mime_type(mime_type: &str) -> Result<(), DataDeviceError> {
    if mime_type.is_empty() || mime_type.len() > DataOfferContract::MAX_MIME_LENGTH {
        return Err(DataDeviceError::InvalidMimeType);
    }
    if mime_type.chars().any(char::is_control) || !mime_type.contains('/') {
        return Err(DataDeviceError::InvalidMimeType);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataDeviceError {
    InvalidMimeType,
    NoMimeTypes,
    TooManyMimeTypes,
    CapabilityNotConfirmed {
        capability: &'static str,
        state: CapabilityState,
    },
}

impl fmt::Display for DataDeviceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMimeType => formatter.write_str("invalid MIME type"),
            Self::NoMimeTypes => formatter.write_str("a data offer needs at least one MIME type"),
            Self::TooManyMimeTypes => formatter.write_str("data offer exceeds MIME type limit"),
            Self::CapabilityNotConfirmed { capability, state } => {
                write!(formatter, "{capability} is {state:?}, not confirmed")
            }
        }
    }
}

impl std::error::Error for DataDeviceError {}

/// Real well-known names and object paths used by portal/notification D-Bus
/// APIs.  They are constants so a future D-Bus transport cannot accept a
/// shell-like free-form command string.
pub mod dbus_contract {
    pub const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
    pub const PORTAL_OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
    pub const PORTAL_DESKTOP_INTERFACE: &str = "org.freedesktop.portal.Desktop";
    pub const PORTAL_PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
    pub const PORTAL_CLIPBOARD_INTERFACE: &str = "org.freedesktop.portal.Clipboard";
    pub const PORTAL_SCREENCAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";
    pub const PORTAL_REMOTE_DESKTOP_INTERFACE: &str = "org.freedesktop.portal.RemoteDesktop";
    pub const PORTAL_REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
    pub const PORTAL_SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
    pub const NOTIFICATIONS_BUS_NAME: &str = "org.freedesktop.Notifications";
    pub const NOTIFICATIONS_OBJECT_PATH: &str = "/org/freedesktop/Notifications";
    pub const NOTIFICATIONS_INTERFACE: &str = "org.freedesktop.Notifications";
}

/// A fixed D-Bus call identifier.  Session object paths and vardict payloads
/// are supplied only by a real D-Bus implementation after user consent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DbusCallContract {
    pub bus_name: &'static str,
    pub object_path: &'static str,
    pub interface: &'static str,
    pub member: &'static str,
    pub requires_session_handle: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DbusOperation {
    PortalDesktopVersion,
    PortalClipboardVersion,
    PortalScreenCastVersion,
    PortalClipboardRequest,
    PortalClipboardSetSelection,
    PortalClipboardSelectionRead,
    PortalClipboardSelectionWrite,
    PortalClipboardSelectionWriteDone,
    PortalScreenCastCreateSession,
    PortalScreenCastSelectSources,
    PortalScreenCastStart,
    PortalScreenCastOpenPipeWireRemote,
    PortalRemoteDesktopCreateSession,
    PortalRemoteDesktopStart,
    PortalRemoteDesktopStop,
    PortalSessionClose,
    NotificationsGetServerInformation,
    NotificationsNotify,
    NotificationsClose,
}

impl DbusOperation {
    pub const fn contract(self) -> DbusCallContract {
        use dbus_contract::*;
        match self {
            Self::PortalDesktopVersion => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_PROPERTIES_INTERFACE,
                member: "Get",
                requires_session_handle: false,
            },
            Self::PortalClipboardVersion => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_PROPERTIES_INTERFACE,
                member: "Get",
                requires_session_handle: false,
            },
            Self::PortalScreenCastVersion => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_PROPERTIES_INTERFACE,
                member: "Get",
                requires_session_handle: false,
            },
            Self::PortalClipboardRequest => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_CLIPBOARD_INTERFACE,
                member: "RequestClipboard",
                requires_session_handle: true,
            },
            Self::PortalClipboardSetSelection => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_CLIPBOARD_INTERFACE,
                member: "SetSelection",
                requires_session_handle: true,
            },
            Self::PortalClipboardSelectionRead => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_CLIPBOARD_INTERFACE,
                member: "SelectionRead",
                requires_session_handle: true,
            },
            Self::PortalClipboardSelectionWrite => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_CLIPBOARD_INTERFACE,
                member: "SelectionWrite",
                requires_session_handle: true,
            },
            Self::PortalClipboardSelectionWriteDone => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_CLIPBOARD_INTERFACE,
                member: "SelectionWriteDone",
                requires_session_handle: true,
            },
            Self::PortalScreenCastCreateSession => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_SCREENCAST_INTERFACE,
                member: "CreateSession",
                requires_session_handle: false,
            },
            Self::PortalScreenCastSelectSources => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_SCREENCAST_INTERFACE,
                member: "SelectSources",
                requires_session_handle: true,
            },
            Self::PortalScreenCastStart => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_SCREENCAST_INTERFACE,
                member: "Start",
                requires_session_handle: true,
            },
            Self::PortalScreenCastOpenPipeWireRemote => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_SCREENCAST_INTERFACE,
                member: "OpenPipeWireRemote",
                requires_session_handle: true,
            },
            Self::PortalRemoteDesktopCreateSession => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_REMOTE_DESKTOP_INTERFACE,
                member: "CreateSession",
                requires_session_handle: false,
            },
            Self::PortalRemoteDesktopStart => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_REMOTE_DESKTOP_INTERFACE,
                member: "Start",
                requires_session_handle: true,
            },
            Self::PortalRemoteDesktopStop => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_REMOTE_DESKTOP_INTERFACE,
                member: "Stop",
                requires_session_handle: true,
            },
            Self::PortalSessionClose => DbusCallContract {
                bus_name: PORTAL_BUS_NAME,
                object_path: PORTAL_OBJECT_PATH,
                interface: PORTAL_SESSION_INTERFACE,
                member: "Close",
                requires_session_handle: true,
            },
            Self::NotificationsGetServerInformation => DbusCallContract {
                bus_name: NOTIFICATIONS_BUS_NAME,
                object_path: NOTIFICATIONS_OBJECT_PATH,
                interface: NOTIFICATIONS_INTERFACE,
                member: "GetServerInformation",
                requires_session_handle: false,
            },
            Self::NotificationsNotify => DbusCallContract {
                bus_name: NOTIFICATIONS_BUS_NAME,
                object_path: NOTIFICATIONS_OBJECT_PATH,
                interface: NOTIFICATIONS_INTERFACE,
                member: "Notify",
                requires_session_handle: false,
            },
            Self::NotificationsClose => DbusCallContract {
                bus_name: NOTIFICATIONS_BUS_NAME,
                object_path: NOTIFICATIONS_OBJECT_PATH,
                interface: NOTIFICATIONS_INTERFACE,
                member: "CloseNotification",
                requires_session_handle: false,
            },
        }
    }
}

#[derive(Debug)]
pub enum XWaylandError {
    NotReady(String),
    InvalidConfig(&'static str),
    Spawn(io::Error),
    MissingDisplayPipe,
    Reader(io::Error),
    Lifecycle(io::Error),
    Shutdown(io::Error),
}

impl fmt::Display for XWaylandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReady(reason) => write!(formatter, "XWayland is not ready: {reason}"),
            Self::InvalidConfig(reason) => write!(formatter, "invalid XWayland configuration: {reason}"),
            Self::Spawn(error) => write!(formatter, "could not spawn XWayland: {error}"),
            Self::MissingDisplayPipe => formatter.write_str("XWayland did not expose its display pipe"),
            Self::Reader(error) => write!(formatter, "could not supervise XWayland display pipe: {error}"),
            Self::Lifecycle(error) => write!(formatter, "could not inspect XWayland lifecycle: {error}"),
            Self::Shutdown(error) => write!(formatter, "could not shut down XWayland: {error}"),
        }
    }
}

impl std::error::Error for XWaylandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) | Self::Reader(error) | Self::Lifecycle(error) | Self::Shutdown(error) => {
                Some(error)
            }
            Self::NotReady(_) | Self::InvalidConfig(_) | Self::MissingDisplayPipe => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos();
            let path = env::temp_dir().join(format!("rouch-compat-{suffix}"));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn xwayland_is_unavailable_without_an_executable() {
        let environment = HostEnvironment {
            wayland_display: Some(OsString::from("wayland-0")),
            runtime_dir: Some(env::temp_dir()),
            ..HostEnvironment::default()
        };
        let adapter = XWaylandAdapter::from_environment(&environment);
        assert_eq!(adapter.detection.state, CapabilityState::Unavailable);
        assert!(matches!(
            adapter.start_or_fallback(),
            XWaylandStart::Fallback { .. }
        ));
    }

    #[test]
    fn xwayland_with_binary_but_no_session_is_degraded() {
        let executable = env::current_exe().expect("test executable");
        let environment = HostEnvironment {
            xwayland_executable: Some(executable),
            ..HostEnvironment::default()
        };
        let adapter = XWaylandAdapter::from_environment(&environment);
        assert_eq!(adapter.detection.state, CapabilityState::Degraded);
        assert!(matches!(
            adapter.start_or_fallback(),
            XWaylandStart::Fallback { .. }
        ));
    }

    #[test]
    fn command_spec_has_fixed_non_shell_arguments_and_environment() {
        let temp = TempDir::new();
        let executable = env::current_exe().expect("test executable");
        let config = XWaylandLaunchConfig::new(
            executable.clone(),
            OsString::from("wayland-0; touch /tmp/injected"),
            temp.path().to_path_buf(),
        )
        .expect("environment values are passed literally, not to a shell");
        let spec = config.command_spec();
        assert_eq!(spec.program, executable);
        assert_eq!(spec.args, vec!["-rootless", "-terminate", "-displayfd", "1"]);
        assert!(spec.args.iter().all(|arg| arg != "sh" && arg != "-c"));
        assert!(
            spec.environment
                .iter()
                .any(|(key, value)| key == "WAYLAND_DISPLAY" && value == "wayland-0; touch /tmp/injected")
        );
    }

    #[test]
    fn empty_path_components_are_not_searched() {
        let path = env::join_paths([Path::new(""), Path::new("/does/not/exist")]).expect("join path");
        assert!(find_executable("Xwayland", Some(&path)).is_none());
    }

    #[test]
    fn data_offer_is_bounded_and_deduplicated() {
        let offer = DataOfferContract::new(
            DataTransferKind::Clipboard,
            [
                "text/plain",
                "text/plain",
                wayland_data_device::PORTAL_FILE_TRANSFER_MIME,
            ],
            Some(7),
        )
        .expect("valid offer");
        assert_eq!(offer.mime_types.len(), 2);
        assert!(offer.accepts("text/plain"));
        assert!(!offer.accepts("image/png"));
    }

    #[test]
    fn data_requests_reject_invalid_mime_and_unconfirmed_transport() {
        assert_eq!(
            DataDeviceRequest::read_clipboard("not-a-mime"),
            Err(DataDeviceError::InvalidMimeType)
        );
        let contract = DataDeviceContract::from_probe(WaylandProbe::default());
        assert!(matches!(
            contract.require_clipboard(),
            Err(DataDeviceError::CapabilityNotConfirmed {
                capability: "clipboard",
                state: CapabilityState::Unavailable
            })
        ));
    }

    #[test]
    fn confirmed_data_device_enables_core_clipboard_and_dnd() {
        let probe = WaylandProbe {
            data_device_manager: ProbeEvidence::Confirmed,
            drag_and_drop: ProbeEvidence::Confirmed,
            ..WaylandProbe::default()
        };
        let matrix = CapabilityMatrix::from_probes(
            &XWaylandDetection {
                state: CapabilityState::Unavailable,
                executable: None,
                wayland_display: None,
                runtime_dir: None,
                reason: "test".to_owned(),
            },
            probe,
            PortalProbe::default(),
        );
        assert_eq!(matrix.state(Capability::DataDevice), CapabilityState::Available);
        assert_eq!(matrix.state(Capability::Clipboard), CapabilityState::Available);
        assert_eq!(matrix.state(Capability::DragAndDrop), CapabilityState::Available);
    }

    #[test]
    fn portal_files_are_degraded_until_a_live_dbus_query_confirms_them() {
        let temp = TempDir::new();
        let services = temp.path().join("dbus-1/services");
        fs::create_dir_all(&services).expect("create service directory");
        fs::write(
            services.join("org.freedesktop.portal.Desktop.service"),
            "[D-BUS Service]\nName=org.freedesktop.portal.Desktop\n",
        )
        .expect("write portal service marker");
        let environment = HostEnvironment {
            dbus_session_bus_address: Some(OsString::from("unix:path=/run/user/1000/bus")),
            data_home: Some(temp.path().to_path_buf()),
            ..HostEnvironment::default()
        };
        let portals = PortalProbe::from_environment(&environment);
        let matrix = CapabilityMatrix::from_probes(
            &XWaylandDetection {
                state: CapabilityState::Unavailable,
                executable: None,
                wayland_display: None,
                runtime_dir: None,
                reason: "test".to_owned(),
            },
            WaylandProbe::default(),
            portals,
        );
        assert_eq!(matrix.state(Capability::DbusSession), CapabilityState::Degraded);
        assert_eq!(matrix.state(Capability::Portals), CapabilityState::Degraded);
        assert_eq!(matrix.state(Capability::Screencast), CapabilityState::Degraded);
        assert_eq!(matrix.state(Capability::Notifications), CapabilityState::Degraded);
    }

    #[test]
    fn confirmed_screencast_and_notifications_use_real_dbus_contracts() {
        let matrix = CapabilityMatrix::from_probes(
            &XWaylandDetection {
                state: CapabilityState::Unavailable,
                executable: None,
                wayland_display: None,
                runtime_dir: None,
                reason: "test".to_owned(),
            },
            WaylandProbe::default(),
            PortalProbe {
                session_bus: ProbeEvidence::Confirmed,
                desktop: ProbeEvidence::Confirmed,
                screencast: ProbeEvidence::Confirmed,
                notifications: ProbeEvidence::Confirmed,
                ..PortalProbe::default()
            },
        );
        assert_eq!(matrix.state(Capability::Screencast), CapabilityState::Available);
        assert_eq!(
            matrix.state(Capability::Notifications),
            CapabilityState::Available
        );
        assert_eq!(
            DbusOperation::PortalScreenCastOpenPipeWireRemote
                .contract()
                .member,
            "OpenPipeWireRemote"
        );
        assert_eq!(
            DbusOperation::NotificationsNotify.contract().bus_name,
            dbus_contract::NOTIFICATIONS_BUS_NAME
        );
    }

    #[test]
    fn local_detection_does_not_claim_wayland_data_device_or_portal_interfaces() {
        let matrix = CapabilityMatrix::detect();
        assert_ne!(
            matrix
                .status(Capability::DataDevice)
                .expect("matrix entry")
                .evidence,
            ProbeEvidence::Confirmed
        );
        assert_ne!(
            matrix
                .status(Capability::Screencast)
                .expect("matrix entry")
                .evidence,
            ProbeEvidence::Confirmed
        );
    }
}
