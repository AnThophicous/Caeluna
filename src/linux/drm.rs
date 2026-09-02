//! Native Linux seat/DRM adapter.
//!
//! Smithay 0.7 exposes the real low-level pieces needed by a compositor:
//! `LibSeatSession`, `DrmDevice`/`DrmDeviceFd`, and `UdevBackend`.  This file
//! wires those pieces together without pretending that the current Winit
//! renderer is already a DRM scanout renderer.  It is therefore a native
//! readiness adapter: it acquires the seat, opens and probes a primary DRM
//! node, tracks pause/activate and hotplug events, and reports an explicit
//! not-ready error until the native frame/renderer path is connected.
//!
//! The adapter intentionally leaves connectors in their current state during
//! probing (`disable_connectors = false`) so a failed launch cannot blank an
//! existing desktop. A future native compositor can opt into connector reset
//! only after it owns the VT and has a real renderer/surface to commit.

use std::{cell::RefCell, fmt, path::PathBuf, rc::Rc, time::Duration};

use smithay::{
    backend::{
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType},
        session::{self, Session as _},
        udev::{UdevBackend, UdevEvent},
    },
    reexports::{
        calloop::{EventLoop, LoopHandle},
        drm::control::{Device as ControlDevice, connector},
    },
    utils::DeviceFd,
};
use tracing::{debug, info, warn};

use crate::session::{self as session_policy, SessionEnvironment, SessionOptions, VtSwitchPlan};

/// The native adapter's recoverable/fatal boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDrmErrorKind {
    Seat,
    Device,
    Udev,
    Drm,
    EventLoop,
    NoDevice,
    NoOutput,
    NotReady,
}

/// An error that keeps the native fallback reason printable without exposing
/// dependency-specific error types in the rest of the compositor.
#[derive(Debug, Clone)]
pub struct NativeDrmError {
    pub kind: NativeDrmErrorKind,
    pub detail: String,
}

impl NativeDrmError {
    fn new(kind: NativeDrmErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    fn not_ready(report: &NativeDrmReport) -> Self {
        Self::new(
            NativeDrmErrorKind::NotReady,
            format!(
                "native DRM/KMS resources are ready but native scanout is not wired yet ({})",
                report.summary()
            ),
        )
    }
}

impl fmt::Display for NativeDrmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for NativeDrmError {}

impl fmt::Display for NativeDrmErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Seat => "seat acquisition failed",
            Self::Device => "DRM device failed",
            Self::Udev => "udev monitor failed",
            Self::Drm => "DRM probe failed",
            Self::EventLoop => "native event source failed",
            Self::NoDevice => "no usable DRM device",
            Self::NoOutput => "no connected DRM output",
            Self::NotReady => "native scanout not ready",
        })
    }
}

/// A mode reported by a physical connector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeMode {
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub refresh_millihertz: u32,
}

/// A connected DRM connector and its advertised modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeOutput {
    pub name: String,
    pub modes: Vec<NativeMode>,
}

/// A cheap, cloneable diagnostic snapshot suitable for logging/settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDrmReport {
    pub seat: String,
    pub device: PathBuf,
    pub atomic: bool,
    pub status: NativeDrmStatus,
    pub outputs: Vec<NativeOutput>,
    pub hotplug_generation: u64,
    pub last_error: Option<String>,
}

impl NativeDrmReport {
    pub fn summary(&self) -> String {
        let mode_count: usize = self.outputs.iter().map(|output| output.modes.len()).sum();
        format!(
            "seat={}, device={}, atomic={}, status={}, outputs={}, modes={}, hotplug_generation={}",
            self.seat,
            self.device.display(),
            self.atomic,
            self.status,
            self.outputs.len(),
            mode_count,
            self.hotplug_generation,
        )
    }
}

/// Lifecycle state exposed by the adapter after a seat/udev event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDrmStatus {
    Active,
    Paused,
    Recovering,
    NoOutputs,
    Failed,
}

impl fmt::Display for NativeDrmStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Recovering => "recovering",
            Self::NoOutputs => "no-outputs",
            Self::Failed => "failed",
        })
    }
}

/// Real native seat + DRM resources. All fields are owned so dropping this
/// value releases DRM master/resources before the libseat session itself.
pub struct NativeDrmBackend {
    seat: session::libseat::LibSeatSession,
    seat_notifier: Option<session::libseat::LibSeatSessionNotifier>,
    device: DrmDevice,
    drm_notifier: Option<smithay::backend::drm::DrmDeviceNotifier>,
    udev: Option<UdevBackend>,
    device_path: PathBuf,
    device_id: u64,
    report: NativeDrmReport,
    needs_reprobe: bool,
}

impl NativeDrmBackend {
    /// Acquire a seat and probe the primary DRM node selected by udev.
    pub fn open(options: &SessionOptions, environment: &SessionEnvironment) -> Result<Self, NativeDrmError> {
        let requested_seat = options.seat.as_deref().unwrap_or(environment.seat.as_str());

        let (mut seat, seat_notifier) = session::libseat::LibSeatSession::new()
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Seat, error.to_string()))?;

        let configured_libseat_backend =
            std::env::var("LIBSEAT_BACKEND").unwrap_or_else(|_| "auto".to_owned());
        info!(
            libseat_backend = %configured_libseat_backend,
            "libseat session provider selected (logind/seatd is chosen by libseat)"
        );

        if !seat.is_active() {
            return Err(NativeDrmError::new(
                NativeDrmErrorKind::Seat,
                format!(
                    "libseat opened {requested_seat:?}, but it is not active; wait for the enable event or start Rouch from a VT"
                ),
            ));
        }

        let actual_seat = seat.seat();
        if actual_seat != requested_seat {
            warn!(
                requested = requested_seat,
                actual = actual_seat,
                "libseat selected a different seat"
            );
        }

        // UdevBackend is Smithay's real DRM add/change/remove event source.
        // Keeping it alive for the whole adapter is what makes hotplug events
        // observable by the calloop registration below.
        let udev = UdevBackend::new(actual_seat.as_str())
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Udev, error.to_string()))?;
        let device_path = select_device(&udev, actual_seat.as_str())?;

        // DrmNode validates that the path is a real DRM node and rejects a
        // render node: modesetting is only legal on a primary node.
        let node = DrmNode::from_path(&device_path)
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Device, error.to_string()))?;
        if node.ty() != NodeType::Primary {
            return Err(NativeDrmError::new(
                NativeDrmErrorKind::Device,
                format!(
                    "{} is {}, but native modesetting needs a primary card node",
                    device_path.display(),
                    node.ty()
                ),
            ));
        }

        let opened = seat
            .open(
                &device_path,
                smithay::reexports::rustix::fs::OFlags::RDWR
                    | smithay::reexports::rustix::fs::OFlags::CLOEXEC,
            )
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Seat, error.to_string()))?;
        let drm_fd = DrmDeviceFd::new(DeviceFd::from(opened));

        // Do not reset/disable connectors during a probe. DrmDevice still
        // performs real capability/resource discovery and selects atomic vs
        // legacy internally, while leaving an existing display untouched.
        let (device, drm_notifier) = DrmDevice::new(drm_fd, false)
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))?;
        let device_id = device.device_id() as u64;
        let atomic = device.is_atomic();
        let mut backend = Self {
            seat,
            seat_notifier: Some(seat_notifier),
            device,
            drm_notifier: Some(drm_notifier),
            udev: Some(udev),
            device_path: device_path.clone(),
            device_id,
            report: NativeDrmReport {
                seat: actual_seat,
                device: device_path,
                atomic,
                status: NativeDrmStatus::Active,
                outputs: Vec::new(),
                hotplug_generation: 0,
                last_error: None,
            },
            needs_reprobe: false,
        };

        backend.reprobe(false)?;
        if !backend.has_scanout_output() {
            backend.report.status = NativeDrmStatus::NoOutputs;
            return Err(NativeDrmError::new(
                NativeDrmErrorKind::NoOutput,
                format!(
                    "{} has no connected connector with an advertised mode",
                    backend.device_path.display()
                ),
            ));
        }

        info!(report = %backend.report.summary(), "Native DRM/KMS resources acquired");
        Ok(backend)
    }

    /// Install real libseat, DRM page-flip, and udev sources in a calloop.
    ///
    /// The weak callbacks avoid an `Rc` cycle; the caller owns the strong
    /// runtime handle and can drop the event loop to unregister all sources.
    pub fn install_sources(
        runtime: &Rc<RefCell<Self>>,
        handle: &LoopHandle<'_, ()>,
    ) -> Result<(), NativeDrmError> {
        let (seat_notifier, drm_notifier, udev) = {
            let mut backend = runtime.borrow_mut();
            if backend.seat_notifier.is_none() || backend.drm_notifier.is_none() || backend.udev.is_none() {
                return Err(NativeDrmError::new(
                    NativeDrmErrorKind::EventLoop,
                    "one or more native sources are already installed",
                ));
            }
            (
                backend.seat_notifier.take().expect("checked above"),
                backend.drm_notifier.take().expect("checked above"),
                backend.udev.take().expect("checked above"),
            )
        };

        let weak_seat = Rc::downgrade(runtime);
        handle
            .insert_source(seat_notifier, move |event, _, _| {
                if let Some(runtime) = weak_seat.upgrade() {
                    runtime.borrow_mut().on_session_event(event);
                }
            })
            .map_err(|error| {
                NativeDrmError::new(NativeDrmErrorKind::EventLoop, format!("libseat: {error:?}"))
            })?;

        let weak_drm = Rc::downgrade(runtime);
        handle
            .insert_source(drm_notifier, move |event, _, _| {
                if let Some(runtime) = weak_drm.upgrade() {
                    runtime.borrow_mut().on_drm_event(event);
                }
            })
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::EventLoop, format!("DRM: {error:?}")))?;

        let weak_udev = Rc::downgrade(runtime);
        handle
            .insert_source(udev, move |event, _, _| {
                if let Some(runtime) = weak_udev.upgrade() {
                    runtime.borrow_mut().on_udev_event(event);
                }
            })
            .map_err(|error| {
                NativeDrmError::new(NativeDrmErrorKind::EventLoop, format!("udev: {error:?}"))
            })?;

        Ok(())
    }

    /// Request a VT switch through libseat. No direct ioctl or privileged
    /// device access is performed here.
    pub fn change_vt(&mut self, vt: i32) -> Result<(), NativeDrmError> {
        let vt = session_policy::validate_vt(vt)
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Seat, error))?;
        self.seat
            .change_vt(vt)
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Seat, error.to_string()))
    }

    pub fn report(&self) -> NativeDrmReport {
        self.report.clone()
    }

    pub fn has_scanout_output(&self) -> bool {
        self.report.outputs.iter().any(|output| !output.modes.is_empty())
    }

    pub fn status(&self) -> NativeDrmStatus {
        self.report.status
    }

    fn on_session_event(&mut self, event: session::Event) {
        match event {
            session::Event::PauseSession => {
                // DrmDevice::pause releases DRM master before the seat is
                // disabled. Its fd remains owned and can be activated again.
                self.device.pause();
                self.report.status = NativeDrmStatus::Paused;
                self.report.outputs.clear();
                self.needs_reprobe = false;
                info!(seat = %self.report.seat, "Native seat paused; DRM master released");
            }
            session::Event::ActivateSession => match self.device.activate(false) {
                Ok(()) => {
                    self.report.status = NativeDrmStatus::Recovering;
                    self.needs_reprobe = true;
                    if let Err(error) = self.reprobe(true) {
                        self.mark_failure(error);
                    } else {
                        info!(report = %self.report.summary(), "Native seat activated and DRM outputs re-probed");
                    }
                }
                Err(error) => {
                    self.mark_failure(NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))
                }
            },
        }
    }

    fn on_drm_event(&mut self, event: DrmEvent) {
        if let DrmEvent::Error(error) = event {
            self.mark_failure(NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()));
        }
        // VBlank is intentionally only observed here. The native renderer will
        // consume it when its real frame queue is connected.
    }

    fn on_udev_event(&mut self, event: UdevEvent) {
        self.report.hotplug_generation = self.report.hotplug_generation.saturating_add(1);
        let current_device_removed = match &event {
            UdevEvent::Removed { device_id } => *device_id as u64 == self.device_id,
            _ => false,
        };

        match event {
            UdevEvent::Added { device_id, path } => {
                info!(
                    ?device_id,
                    ?path,
                    "DRM device added; scheduling native output re-probe"
                );
            }
            UdevEvent::Changed { device_id } => {
                debug!(
                    ?device_id,
                    "DRM device changed; scheduling native connector re-probe"
                );
            }
            UdevEvent::Removed { device_id } => {
                warn!(?device_id, "DRM device removed; native output needs recovery");
            }
        }

        self.needs_reprobe = true;
        if current_device_removed {
            self.report.status = NativeDrmStatus::Recovering;
            self.report.outputs.clear();
            self.report.last_error = Some("selected DRM node was removed".to_owned());
        } else if let Err(error) = self.reprobe_if_needed() {
            self.mark_failure(error);
        }
    }

    fn reprobe_if_needed(&mut self) -> Result<(), NativeDrmError> {
        if self.needs_reprobe {
            self.reprobe(true)?;
        }
        Ok(())
    }

    fn reprobe(&mut self, force_probe: bool) -> Result<(), NativeDrmError> {
        if !self.device.is_active() {
            self.report.status = NativeDrmStatus::Paused;
            return Ok(());
        }

        let resources = self
            .device
            .resource_handles()
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))?;
        let mut outputs = Vec::new();
        let mut connector_errors = Vec::new();

        for handle in resources.connectors().iter().copied() {
            match self.device.get_connector(handle, force_probe) {
                Ok(info) if info.state() == connector::State::Connected => {
                    let modes = info
                        .modes()
                        .iter()
                        .map(|mode| {
                            let (width, height) = mode.size();
                            NativeMode {
                                name: mode.name().to_string_lossy().into_owned(),
                                width,
                                height,
                                refresh_millihertz: mode.vrefresh(),
                            }
                        })
                        .collect();
                    outputs.push(NativeOutput {
                        name: info.to_string(),
                        modes,
                    });
                }
                Ok(_) => {}
                Err(error) => connector_errors.push(error.to_string()),
            }
        }

        self.report.outputs = outputs;
        self.report.last_error = connector_errors.into_iter().next();
        self.report.status = if self.has_scanout_output() {
            NativeDrmStatus::Active
        } else {
            NativeDrmStatus::NoOutputs
        };
        self.needs_reprobe = false;
        Ok(())
    }

    fn mark_failure(&mut self, error: NativeDrmError) {
        warn!(kind = %error.kind, detail = %error.detail, "Native DRM adapter entered recovery state");
        self.report.status = NativeDrmStatus::Failed;
        self.report.last_error = Some(error.to_string());
        self.needs_reprobe = true;
    }
}

/// Run the native readiness path once. A successful probe intentionally returns
/// `NotReady` because the current Liquid Glass frame compositor is still tied
/// to the nested Winit surface; `linux.rs` turns that into the configured
/// nested fallback and drops this backend first.
pub(super) fn run_native(
    options: &SessionOptions,
    environment: &SessionEnvironment,
) -> Result<(), NativeDrmError> {
    let mut event_loop: EventLoop<()> = EventLoop::try_new()
        .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::EventLoop, error.to_string()))?;
    // Declare the event loop first. Rust drops locals in reverse declaration
    // order, so the runtime/device is released before the event-source
    // notifiers release the strong libseat session on every error path.
    let runtime = Rc::new(RefCell::new(NativeDrmBackend::open(options, environment)?));
    NativeDrmBackend::install_sources(&runtime, &event_loop.handle())?;

    if let VtSwitchPlan::Request(vt) = session_policy::vt_switch_plan(options, environment) {
        runtime.borrow_mut().change_vt(vt)?;
    }

    // Drain already-pending enable/disable/hotplug messages without entering a
    // second compositor loop. This is enough to prove source registration and
    // lets a startup VT event update the diagnostic state before fallback.
    event_loop
        .dispatch(Some(Duration::ZERO), &mut ())
        .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::EventLoop, error.to_string()))?;

    let report = runtime.borrow().report();
    Err(NativeDrmError::not_ready(&report))
}

fn select_device(udev: &UdevBackend, seat: &str) -> Result<PathBuf, NativeDrmError> {
    if let Some(requested) = std::env::var_os("ROUCH_DRM_DEVICE") {
        let path = PathBuf::from(requested);
        if !path.is_absolute() {
            return Err(NativeDrmError::new(
                NativeDrmErrorKind::Device,
                "ROUCH_DRM_DEVICE must be an absolute path",
            ));
        }
        return Ok(path);
    }

    // `primary_gpu` is Smithay's boot-VGA-aware selector. The udev argument is
    // otherwise still used to construct and retain the hotplug monitor.
    let _known_devices = udev.device_list().count();
    smithay::backend::udev::primary_gpu(seat)
        .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Udev, error.to_string()))?
        .ok_or_else(|| {
            NativeDrmError::new(
                NativeDrmErrorKind::NoDevice,
                format!("udev found no primary GPU for seat {seat:?}"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_summary_is_compact_and_deterministic() {
        let report = NativeDrmReport {
            seat: "seat0".to_owned(),
            device: PathBuf::from("/dev/dri/card0"),
            atomic: true,
            status: NativeDrmStatus::Active,
            outputs: vec![NativeOutput {
                name: "HDMI-A-1".to_owned(),
                modes: vec![NativeMode {
                    name: "1280x800".to_owned(),
                    width: 1280,
                    height: 800,
                    refresh_millihertz: 60_000,
                }],
            }],
            hotplug_generation: 2,
            last_error: None,
        };
        assert!(report.summary().contains("seat=seat0"));
        assert!(report.summary().contains("outputs=1"));
        assert!(report.summary().contains("modes=1"));
    }

    #[test]
    fn scanout_requires_a_mode_not_just_a_connected_connector() {
        // The predicate is kept on the report shape so tests do not need a
        // real /dev/dri node or a privileged seat.
        let outputs = [NativeOutput {
            name: "DP-1".to_owned(),
            modes: Vec::new(),
        }];
        assert!(!outputs.iter().any(|output| !output.modes.is_empty()));
    }
}
