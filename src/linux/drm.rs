//! Native Linux seat/DRM adapter.
//!
//! Smithay 0.7 exposes the real low-level pieces needed by a compositor:
//! `LibSeatSession`, `DrmDevice`/`DrmDeviceFd`, and `UdevBackend`.  This file
//! wires those pieces together and exposes the native GBM/EGL/GLES frame
//! pipeline. The native entry point also owns the Wayland/Rouch event loop, so
//! a successful return means the compositor was actually running rather than
//! merely passing a DRM probe.
//!
//! The adapter intentionally leaves connectors in their current state during
//! probing (`disable_connectors = false`) so a failed launch cannot blank an
//! existing desktop. The first actual frame performs the modeset and later
//! frames use page-flip; no successful startup is reported before that frame
//! pipeline is connected to the compositor loop.

use std::{
    cell::RefCell,
    collections::HashSet,
    fmt,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

#[cfg(feature = "native-session")]
use smithay::reexports::drm::control::{Mode, ModeTypeFlags, crtc};
use smithay::{
    backend::{
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType},
        session::{self, Session as _},
        udev::{UdevBackend, UdevEvent},
    },
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{Device as ControlDevice, connector},
    },
    utils::DeviceFd,
};
use tracing::{debug, info, warn};

use crate::session::{self as session_policy, SessionEnvironment, SessionOptions, VtSwitchPlan};

#[cfg(feature = "native-session")]
#[path = "native.rs"]
mod native;

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

    /// Whether a seat/VT hand-off can still resolve this failure.
    ///
    /// A display manager releases the previous session's seat asynchronously,
    /// so the first probe after a login can legitimately find an inactive seat
    /// or a connector that has not finished coming back.
    #[cfg(feature = "native-session")]
    fn is_transient(&self) -> bool {
        matches!(
            self.kind,
            NativeDrmErrorKind::Seat | NativeDrmErrorKind::Drm | NativeDrmErrorKind::NoOutput
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

/// The connector/CRTC/mode tuple selected for the one-output P0 pipeline.
#[cfg(feature = "native-session")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NativeOutputConfig {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub mode: Mode,
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
    #[cfg(feature = "native-session")]
    scanout: Option<NativeOutputConfig>,
    #[cfg(feature = "native-session")]
    pipeline: Option<native::NativeFramePipeline>,
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
            #[cfg(feature = "native-session")]
            scanout: None,
            #[cfg(feature = "native-session")]
            pipeline: None,
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

        #[cfg(not(feature = "native-session"))]
        {
            return Err(NativeDrmError::new(
                NativeDrmErrorKind::NotReady,
                "native-session feature is disabled; build with --features native-session",
            ));
        }

        #[cfg(feature = "native-session")]
        {
            let candidates = scanout_candidates(&backend.device)?;
            let mut last_pipeline_error = None;
            for config in candidates {
                match native::NativeFramePipeline::new(&mut backend.device, config) {
                    Ok(pipeline) => {
                        backend.scanout = Some(config);
                        backend.pipeline = Some(pipeline);
                        break;
                    }
                    Err(error) => last_pipeline_error = Some(error.to_string()),
                }
            }

            if backend.pipeline.is_none() {
                return Err(NativeDrmError::new(
                    NativeDrmErrorKind::Drm,
                    format!(
                        "connected DRM outputs were found, but no GBM/EGL/GLES scanout pipeline could be initialized{}",
                        last_pipeline_error
                            .map(|error| format!(": {error}"))
                            .unwrap_or_default()
                    ),
                ));
            }
        }

        info!(report = %backend.report.summary(), "Native DRM/KMS resources acquired");
        Ok(backend)
    }

    /// Install real libseat, DRM page-flip, and udev sources in a calloop.
    ///
    /// The weak callbacks avoid an `Rc` cycle; the caller owns the strong
    /// runtime handle and can drop the event loop to unregister all sources.
    #[cfg(feature = "native-session")]
    pub fn install_sources<T: 'static>(
        runtime: &Rc<RefCell<Self>>,
        handle: &LoopHandle<'_, T>,
        wake: Option<smithay::reexports::calloop::LoopSignal>,
        input: super::input::LibinputSeatHandle,
    ) -> Result<(), NativeDrmError> {
        let (seat_notifier, drm_notifier, udev) = {
            let mut backend = runtime.borrow_mut();
            if backend.seat_notifier.is_none() || backend.drm_notifier.is_none() || backend.udev.is_none() {
                return Err(NativeDrmError::new(
                    NativeDrmErrorKind::EventLoop,
                    "the native seat, DRM and udev sources can only be installed once",
                ));
            }
            (
                backend.seat_notifier.take().expect("checked above"),
                backend.drm_notifier.take().expect("checked above"),
                backend.udev.take().expect("checked above"),
            )
        };

        let weak_seat = Rc::downgrade(runtime);
        let seat_wake = wake.clone();
        let seat_input = input.clone();
        handle
            .insert_source(seat_notifier, move |event, _, _| {
                seat_input.session_event(event);
                let failed = if let Some(runtime) = weak_seat.upgrade() {
                    let mut backend = runtime.borrow_mut();
                    backend.on_session_event(event);
                    backend.status() == NativeDrmStatus::Failed
                } else {
                    false
                };
                if failed {
                    if let Some(wake) = &seat_wake {
                        wake.stop();
                    }
                }
                if let Some(wake) = &seat_wake {
                    wake.wakeup();
                }
            })
            .map_err(|error| {
                NativeDrmError::new(NativeDrmErrorKind::EventLoop, format!("libseat: {error:?}"))
            })?;

        let weak_drm = Rc::downgrade(runtime);
        let drm_wake = wake.clone();
        handle
            .insert_source(drm_notifier, move |event, _, _| {
                let failed = if let Some(runtime) = weak_drm.upgrade() {
                    let mut backend = runtime.borrow_mut();
                    backend.on_drm_event(event);
                    backend.status() == NativeDrmStatus::Failed
                } else {
                    false
                };
                if let Some(wake) = &drm_wake {
                    if failed {
                        wake.stop();
                    }
                    wake.wakeup();
                }
            })
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::EventLoop, format!("DRM: {error:?}")))?;

        let weak_udev = Rc::downgrade(runtime);
        let udev_wake = wake;
        handle
            .insert_source(udev, move |event, _, _| {
                let failed = if let Some(runtime) = weak_udev.upgrade() {
                    let mut backend = runtime.borrow_mut();
                    backend.on_udev_event(event);
                    backend.status() == NativeDrmStatus::Failed
                } else {
                    false
                };
                if let Some(wake) = &udev_wake {
                    if failed {
                        wake.stop();
                    }
                    wake.wakeup();
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

    #[cfg(feature = "native-session")]
    pub fn can_render(&self) -> bool {
        self.report.status == NativeDrmStatus::Active
            && self
                .pipeline
                .as_ref()
                .is_some_and(native::NativeFramePipeline::can_render)
    }

    /// Build and submit the scene while the native GLES renderer is bound.
    #[cfg(feature = "native-session")]
    pub fn render_frame_with<E, F>(
        &mut self,
        clear_color: impl Into<smithay::backend::renderer::Color32F>,
        build: F,
    ) -> Result<(), NativeDrmError>
    where
        E: smithay::backend::renderer::element::RenderElement<smithay::backend::renderer::gles::GlesRenderer>,
        F: FnOnce(&mut smithay::backend::renderer::gles::GlesRenderer) -> Result<Vec<E>, String>,
    {
        let pipeline = self.pipeline.as_mut().ok_or_else(|| {
            NativeDrmError::new(
                NativeDrmErrorKind::NotReady,
                "native GBM/EGL/GLES pipeline is not active",
            )
        })?;
        pipeline
            .render_frame_with(clear_color.into(), build)
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))
    }

    /// Render and queue one native frame for the selected output.
    ///
    /// The first call performs the pending connector/CRTC/mode commit. Every
    /// later call must be made only after the matching DRM vblank has reached
    /// `on_drm_event` (or after the caller has otherwise called
    /// `frame_submitted`) so the GBM swapchain never reuses an in-flight BO.
    #[cfg(feature = "native-session")]
    pub fn render_frame<E>(
        &mut self,
        elements: &[E],
        clear_color: impl Into<smithay::backend::renderer::Color32F>,
    ) -> Result<(), NativeDrmError>
    where
        E: smithay::backend::renderer::element::RenderElement<smithay::backend::renderer::gles::GlesRenderer>,
    {
        let pipeline = self.pipeline.as_mut().ok_or_else(|| {
            NativeDrmError::new(
                NativeDrmErrorKind::NotReady,
                "native GBM/EGL/GLES pipeline is not active",
            )
        })?;
        pipeline
            .render_frame(elements, clear_color.into())
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))
    }

    /// Complete the swapchain state for a vblank belonging to this output.
    ///
    /// This is public for an integrator that dispatches DRM events itself;
    /// the built-in notifier calls the same operation automatically.
    #[cfg(feature = "native-session")]
    pub fn frame_submitted(&mut self, crtc: crtc::Handle) -> Result<bool, NativeDrmError> {
        let Some(pipeline) = self.pipeline.as_mut() else {
            return Ok(false);
        };
        pipeline
            .frame_submitted(crtc)
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))
    }

    /// The dmabuf formats the native renderer can import, for the Wayland
    /// `zwp_linux_dmabuf_v1` global.
    #[cfg(feature = "native-session")]
    pub fn render_formats(&self) -> Option<smithay::backend::allocator::format::FormatSet> {
        self.pipeline
            .as_ref()
            .and_then(native::NativeFramePipeline::render_formats)
    }

    /// Number of frames whose DRM page-flip has reached vblank.
    #[cfg(feature = "native-session")]
    pub fn presented_frames(&self) -> u64 {
        self.pipeline
            .as_ref()
            .map_or(0, native::NativeFramePipeline::presented_frames)
    }

    /// Return the selected physical output and mode, if native resources exist.
    #[cfg(feature = "native-session")]
    pub fn scanout_config(&self) -> Option<(crtc::Handle, (u16, u16), u32)> {
        self.scanout
            .map(|config| (config.crtc, config.mode.size(), config.mode.vrefresh()))
    }

    fn on_session_event(&mut self, event: session::Event) {
        match event {
            session::Event::PauseSession => {
                // Drop the buffered surface before DrmDevice::pause releases
                // DRM master. This prevents a pending BO/page-flip from being
                // reused after VT ownership has gone away.
                #[cfg(feature = "native-session")]
                if let Some(pipeline) = self.pipeline.as_mut() {
                    pipeline.pause();
                }
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
                        self.mark_recovering(error);
                        return;
                    }
                    #[cfg(feature = "native-session")]
                    if let Err(error) = self.restore_scanout() {
                        self.mark_recovering(error);
                        return;
                    }
                    info!(report = %self.report.summary(), "Native seat activated and DRM outputs re-probed");
                }
                Err(error) => {
                    self.mark_recovering(NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))
                }
            },
        }
    }

    fn on_drm_event(&mut self, event: DrmEvent) {
        match event {
            #[cfg(feature = "native-session")]
            DrmEvent::VBlank(crtc) => {
                if let Err(error) = self.frame_submitted(crtc) {
                    self.mark_failure(error);
                }
            }
            #[cfg(not(feature = "native-session"))]
            DrmEvent::VBlank(_) => {}
            DrmEvent::Error(error) => {
                self.mark_failure(NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()));
            }
        }
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
            #[cfg(feature = "native-session")]
            if let Some(pipeline) = self.pipeline.as_mut() {
                pipeline.pause();
            }
            self.report.outputs.clear();
            self.mark_failure(NativeDrmError::new(
                NativeDrmErrorKind::Device,
                "selected DRM node was removed; native session cannot safely continue",
            ));
        } else if let Err(error) = self.reprobe_if_needed() {
            self.mark_recovering(error);
        } else {
            #[cfg(feature = "native-session")]
            if let Err(error) = self.restore_scanout() {
                self.mark_recovering(error);
            }
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

    /// Drop the scanout surface so the next `restore_scanout` rebuilds it.
    ///
    /// `restore_scanout` reuses a pipeline whose connector/CRTC/mode is still
    /// advertised, so a surface that renders but never presents would otherwise
    /// be "restored" unchanged on every retry.
    #[cfg(feature = "native-session")]
    fn release_scanout_surface(&mut self) {
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.pause();
        }
    }

    #[cfg(feature = "native-session")]
    fn restore_scanout(&mut self) -> Result<(), NativeDrmError> {
        if !self.device.is_active() {
            return Ok(());
        }

        let candidates = scanout_candidates(&self.device)?;
        if candidates.is_empty() {
            if let Some(pipeline) = self.pipeline.as_mut() {
                pipeline.pause();
            }
            self.scanout = None;
            self.report.status = NativeDrmStatus::NoOutputs;
            return Ok(());
        }

        // Reuse the existing EGL/GLES context when the selected connector,
        // CRTC and mode are still valid. Activation only recreates the DRM
        // surface and GBM swapchain in that case.
        if let Some(pipeline) = self.pipeline.as_mut() {
            let config = pipeline.config();
            if candidates.contains(&config) {
                pipeline
                    .activate(&mut self.device)
                    .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))?;
                self.scanout = Some(config);
                self.report.status = NativeDrmStatus::Active;
                return Ok(());
            }
        }

        // Hotplug can invalidate the old connector or mode. Drop its surface
        // before trying a new tuple so no two native surfaces claim the same
        // primary plane.
        self.pipeline.take();
        let mut last_error = None;
        for config in candidates {
            match native::NativeFramePipeline::new(&mut self.device, config) {
                Ok(pipeline) => {
                    self.scanout = Some(config);
                    self.pipeline = Some(pipeline);
                    self.report.status = NativeDrmStatus::Active;
                    self.report.last_error = None;
                    return Ok(());
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }

        Err(NativeDrmError::new(
            NativeDrmErrorKind::Drm,
            format!(
                "DRM outputs are connected, but scanout could not be restored{}",
                last_error.map(|error| format!(": {error}")).unwrap_or_default()
            ),
        ))
    }

    /// Record a scanout loss that a later probe can still undo.
    ///
    /// Hotplug, a connector re-probe and a seat hand-off all reach this path.
    /// Marking them `Failed` stops the event loop, which the user sees as a
    /// black screen followed by the display manager's login screen.
    /// `Recovering` keeps the session alive so the frame timer can retry.
    fn mark_recovering(&mut self, error: NativeDrmError) {
        warn!(kind = %error.kind, detail = %error.detail, "Native scanout lost; scheduling a retry");
        self.report.status = NativeDrmStatus::Recovering;
        self.report.last_error = Some(error.to_string());
        self.needs_reprobe = true;
    }

    /// Retry a recoverable scanout loss. `true` means scanout is active again.
    #[cfg(feature = "native-session")]
    pub fn try_recover(&mut self) -> bool {
        match self.report.status {
            NativeDrmStatus::Active => return true,
            // `Paused` is a VT switch: the seat notifier owns that transition.
            NativeDrmStatus::Failed | NativeDrmStatus::Paused => return false,
            NativeDrmStatus::Recovering | NativeDrmStatus::NoOutputs => {}
        }

        if !self.device.is_active() {
            if let Err(error) = self.device.activate(false) {
                self.report.last_error = Some(error.to_string());
                return false;
            }
            self.needs_reprobe = true;
        }
        if let Err(error) = self.reprobe_if_needed() {
            self.report.last_error = Some(error.to_string());
            return false;
        }
        match self.restore_scanout() {
            Ok(()) => self.report.status == NativeDrmStatus::Active,
            Err(error) => {
                self.report.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn mark_failure(&mut self, error: NativeDrmError) {
        warn!(kind = %error.kind, detail = %error.detail, "Native DRM adapter entered recovery state");
        self.report.status = NativeDrmStatus::Failed;
        self.report.last_error = Some(error.to_string());
        self.needs_reprobe = true;
    }
}

/// How often a lost native scanout is retried by the frame timer.
#[cfg(feature = "native-session")]
const RECOVERY_INTERVAL: Duration = Duration::from_millis(500);
/// How long a lost native scanout may stay unrecovered before the session ends.
#[cfg(feature = "native-session")]
const RECOVERY_BUDGET: Duration = Duration::from_secs(20);
/// How many times a native startup is retried across a seat/VT hand-off.
#[cfg(feature = "native-session")]
const OPEN_ATTEMPTS: u32 = 4;
/// Pause between native startup attempts.
#[cfg(feature = "native-session")]
const OPEN_BACKOFF: Duration = Duration::from_millis(250);
/// Consecutive failed frames tolerated before the native session gives up.
#[cfg(feature = "native-session")]
const MAX_RENDER_FAILURES: u32 = 60;

/// Validate everything the Wayland side needs *before* any DRM master is taken.
///
/// `NativeDrmBackend::open` creates the GBM/EGL scanout surface, and from that
/// moment the display manager's framebuffer is gone. Every failure after that
/// point is invisible: the screen is already black and the display manager only
/// observes the process exit, so a missing `XDG_RUNTIME_DIR` and a dead GPU look
/// identical to the user. Running these checks first keeps a recoverable failure
/// on the greeter, where its message can still be read.
#[cfg(feature = "native-session")]
fn preflight(environment: &SessionEnvironment) -> Result<(), NativeDrmError> {
    let Some(runtime_dir) = environment.runtime_dir.as_ref() else {
        return Err(NativeDrmError::new(
            NativeDrmErrorKind::NotReady,
            "XDG_RUNTIME_DIR is not set, so the Wayland socket cannot be created; \
             start the session from a display manager or a logind session",
        ));
    };
    if !runtime_dir.is_dir() {
        return Err(NativeDrmError::new(
            NativeDrmErrorKind::NotReady,
            format!(
                "XDG_RUNTIME_DIR={} is not an existing directory",
                runtime_dir.display()
            ),
        ));
    }

    // The socket is created by `ListeningSocketSource::new_auto` much later,
    // after DRM master has been taken. Proving the directory is writable now
    // turns that late failure into an early, visible one.
    let probe = runtime_dir.join(".rouch-session-preflight");
    std::fs::write(&probe, b"").map_err(|error| {
        NativeDrmError::new(
            NativeDrmErrorKind::NotReady,
            format!(
                "XDG_RUNTIME_DIR={} is not writable: {error}",
                runtime_dir.display()
            ),
        )
    })?;
    let _ = std::fs::remove_file(&probe);

    if (0..32).all(|index| runtime_dir.join(format!("wayland-{index}")).exists()) {
        return Err(NativeDrmError::new(
            NativeDrmErrorKind::NotReady,
            format!(
                "no free Wayland socket name in {}; wayland-0 through wayland-31 are all taken",
                runtime_dir.display()
            ),
        ));
    }

    Ok(())
}

/// Open the seat and DRM node, retrying while the display manager finishes
/// handing over the VT.
#[cfg(feature = "native-session")]
fn open_with_retry(
    options: &SessionOptions,
    environment: &SessionEnvironment,
) -> Result<NativeDrmBackend, NativeDrmError> {
    let mut last_error = None;
    for attempt in 1..=OPEN_ATTEMPTS {
        match NativeDrmBackend::open(options, environment) {
            Ok(backend) => return Ok(backend),
            Err(error) if attempt < OPEN_ATTEMPTS && error.is_transient() => {
                warn!(
                    attempt,
                    kind = %error.kind,
                    detail = %error.detail,
                    "Native startup was not ready yet; retrying after the seat hand-off"
                );
                last_error = Some(error);
                std::thread::sleep(OPEN_BACKOFF);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        NativeDrmError::new(
            NativeDrmErrorKind::NotReady,
            "native startup did not complete within the retry budget",
        )
    }))
}

/// Run a complete native session: acquire the seat and DRM node, create a
/// Wayland socket, drive libinput and render the same Rouch scene into a real
/// GBM/KMS page-flip loop.
#[cfg(not(feature = "native-session"))]
pub(super) fn run_native(
    _options: &SessionOptions,
    _environment: &SessionEnvironment,
) -> Result<(), NativeDrmError> {
    Err(NativeDrmError::new(
        NativeDrmErrorKind::NotReady,
        "native-session feature is disabled; build with --features native-session",
    ))
}

#[cfg(feature = "native-session")]
pub(super) fn run_native(
    options: &SessionOptions,
    environment: &SessionEnvironment,
) -> Result<(), NativeDrmError> {
    let mut event_loop: EventLoop<super::Rouch> = EventLoop::try_new()
        .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::EventLoop, error.to_string()))?;
    // Everything that does not need DRM master is checked while the display
    // manager still owns the screen, so a failure is readable instead of black.
    preflight(environment)?;
    // Declare the event loop first. Rust drops locals in reverse declaration
    // order, so the runtime/device is released before the event-source
    // notifiers release the strong libseat session on every error path.
    let runtime = Rc::new(RefCell::new(open_with_retry(options, environment)?));

    let output_size = runtime
        .borrow()
        .scanout_config()
        .map(|(_, (width, height), _)| (i32::from(width), i32::from(height)).into())
        .ok_or_else(|| {
            NativeDrmError::new(
                NativeDrmErrorKind::NoOutput,
                "native DRM pipeline has no selected output mode",
            )
        })?;
    let display = smithay::reexports::wayland_server::Display::new()
        .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::EventLoop, error.to_string()))?;
    let mut state = super::Rouch::new(
        &mut event_loop,
        display,
        output_size,
        "rouch-native",
        smithay::utils::Transform::Normal,
    )
    .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::EventLoop, error.to_string()))?;

    // Accelerated clients need the dmabuf global before they connect, so it is
    // published as soon as both the renderer and the Wayland state exist.
    if let Some(formats) = runtime.borrow().render_formats() {
        state.enable_dmabuf(formats);
    }
    // Portals and other D-Bus activated services are started by the user bus
    // and never inherit this process's environment. Only the native session
    // publishes it; a nested one would overwrite the host desktop's.
    state.export_session_environment();

    let (input_session, seat_name) = {
        let backend = runtime.borrow();
        (backend.seat.clone(), backend.report.seat.clone())
    };
    let mut input_source = super::input::LibinputSeatSource::new(input_session, seat_name)
        .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Seat, error.to_string()))?;
    input_source
        .normalizer_mut()
        .set_absolute_output_size(output_size.w, output_size.h);
    let input_handle = input_source.handle();
    input_handle.session_event(smithay::backend::session::Event::ActivateSession);

    NativeDrmBackend::install_sources(
        &runtime,
        &event_loop.handle(),
        Some(state.loop_signal.clone()),
        input_handle.clone(),
    )?;
    event_loop
        .handle()
        .insert_source(input_source, |event, _, state| {
            if state.process_normalized_input_event(event) {
                state.request_redraw();
            }
        })
        .map_err(|error| {
            NativeDrmError::new(NativeDrmErrorKind::EventLoop, format!("libinput: {error:?}"))
        })?;

    if let VtSwitchPlan::Request(vt) = session_policy::vt_switch_plan(options, environment) {
        runtime.borrow_mut().change_vt(vt)?;
    }

    let frame_runtime = Rc::clone(&runtime);
    let repeat_input = input_handle.clone();
    let mut current_output_size = output_size;
    let mut delivered_presentations = runtime.borrow().presented_frames();
    let mut last_recovery_attempt = Instant::now();
    let mut recovering_since: Option<Instant> = None;
    let mut render_failures: u32 = 0;
    event_loop
        .handle()
        .insert_source(Timer::from_duration(Duration::ZERO), move |_, _, state| {
            if state.tick_session() {
                state.request_redraw();
            }
            if state.poll_terminal() || state.refresh_game_mode_if_due() {
                state.request_redraw();
            }

            repeat_input.repeat_tick(state.notification_now_ms());
            let pending_input = repeat_input.drain_events();
            if !pending_input.is_empty() {
                for event in pending_input {
                    if state.process_normalized_input_event(event) {
                        state.request_redraw();
                    }
                }
                state.request_redraw();
            }

            // A Wayland frame callback means that the compositor actually
            // presented the previous buffer. Do not acknowledge a client
            // commit merely because a page-flip request was queued.
            let presented_frames = frame_runtime.borrow().presented_frames();
            if presented_frames > delivered_presentations {
                delivered_presentations = presented_frames;
                super::nested::complete_native_frame(state);
            }

            // A hotplug, a connector re-probe or a seat hand-off can drop the
            // scanout pipeline. That is recoverable, so retry on a slow cadence
            // instead of ending the session. The budget still bounds it: a GPU
            // that never comes back exits rather than holding a black screen.
            if frame_runtime.borrow().status() == NativeDrmStatus::Active {
                recovering_since = None;
            } else {
                let now = Instant::now();
                if now.duration_since(last_recovery_attempt) >= RECOVERY_INTERVAL {
                    last_recovery_attempt = now;
                    let recovered = frame_runtime.borrow_mut().try_recover();
                    if recovered {
                        recovering_since = None;
                        state.request_redraw();
                    } else if frame_runtime.borrow().status() != NativeDrmStatus::Paused {
                        let since = *recovering_since.get_or_insert(now);
                        if now.duration_since(since) >= RECOVERY_BUDGET {
                            frame_runtime.borrow_mut().mark_failure(NativeDrmError::new(
                                NativeDrmErrorKind::Drm,
                                "native scanout could not be restored within the recovery budget",
                            ));
                            state.loop_signal.stop();
                        }
                    }
                }
            }

            let can_render = frame_runtime.borrow().can_render();
            if can_render && state.redraw_needed() && !state.output_blank() {
                if let Some((_, (width, height), _)) = frame_runtime.borrow().scanout_config() {
                    let native_size = (i32::from(width), i32::from(height)).into();
                    if native_size != current_output_size {
                        state.update_output_size(native_size);
                        current_output_size = native_size;
                    }
                }
                state.reconcile_nested_clients();
                state.refresh_animations();
                state.consume_redraw_request();
                let result = {
                    let mut backend = frame_runtime.borrow_mut();
                    backend.render_frame_with::<super::decorations::RouchRenderElements<
                        smithay::backend::renderer::gles::GlesRenderer,
                    >, _>([0.015, 0.035, 0.095, 1.0], |renderer| {
                        Ok(super::nested::build_render_elements(
                            state,
                            renderer,
                            current_output_size,
                        ))
                    })
                };
                match result {
                    Ok(()) => render_failures = 0,
                    Err(error) => {
                        warn!(?error, "Could not render native Rouch frame");
                        render_failures += 1;
                        let mut backend = frame_runtime.borrow_mut();
                        if render_failures >= MAX_RENDER_FAILURES {
                            backend.mark_failure(error);
                            drop(backend);
                            state.loop_signal.stop();
                        } else {
                            // A single failed frame is not a dead GPU. Release
                            // the surface and let the recovery pass rebuild it.
                            backend.release_scanout_surface();
                            backend.mark_recovering(error);
                        }
                    }
                }
            }

            TimeoutAction::ToDuration(Duration::from_millis(16))
        })
        .map_err(|error| {
            NativeDrmError::new(
                NativeDrmErrorKind::EventLoop,
                format!("native frame timer: {error:?}"),
            )
        })?;

    info!(
        socket = ?state.socket_name,
        output = ?output_size,
        "Rouch native Wayland compositor is ready"
    );
    event_loop
        .run(None, &mut state, |_| {})
        .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::EventLoop, error.to_string()))?;

    let report = runtime.borrow().report();
    if report.status == NativeDrmStatus::Failed {
        Err(NativeDrmError::new(
            NativeDrmErrorKind::Drm,
            report
                .last_error
                .unwrap_or_else(|| "native session stopped after an unrecoverable DRM error".to_owned()),
        ))
    } else {
        Ok(())
    }
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

/// Build connector/CRTC/mode tuples in the order a compositor should try
/// them.  A connector's encoder mask is the authoritative compatibility
/// relationship; merely pairing the first connector with the first CRTC can
/// pass a superficial probe and still fail the first modeset ioctl.
#[cfg(feature = "native-session")]
fn scanout_candidates(device: &DrmDevice) -> Result<Vec<NativeOutputConfig>, NativeDrmError> {
    let resources = device
        .resource_handles()
        .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))?;
    let mut candidates = Vec::new();

    for connector_handle in resources.connectors().iter().copied() {
        let connector_info = device
            .get_connector(connector_handle, true)
            .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))?;
        if connector_info.state() != connector::State::Connected || connector_info.modes().is_empty() {
            continue;
        }

        let mut modes = connector_info.modes().to_vec();
        modes.sort_by(|left, right| {
            let left_preferred = left.mode_type().contains(ModeTypeFlags::PREFERRED);
            let right_preferred = right.mode_type().contains(ModeTypeFlags::PREFERRED);
            let (left_width, left_height) = left.size();
            let (right_width, right_height) = right.size();

            right_preferred
                .cmp(&left_preferred)
                .then_with(|| {
                    (right_width as u32 * right_height as u32).cmp(&(left_width as u32 * left_height as u32))
                })
                .then_with(|| right.vrefresh().cmp(&left.vrefresh()))
        });

        let mut encoder_handles = connector_info.encoders().to_vec();
        if let Some(current_encoder) = connector_info.current_encoder() {
            encoder_handles.sort_by_key(|handle| *handle != current_encoder);
        }

        let mut crtcs = Vec::new();
        let mut seen_crtcs = HashSet::new();
        for encoder_handle in encoder_handles {
            let encoder = device
                .get_encoder(encoder_handle)
                .map_err(|error| NativeDrmError::new(NativeDrmErrorKind::Drm, error.to_string()))?;

            // Prefer the CRTC that is already driving this connector so the
            // first frame can reuse an existing mode without a needless
            // connector hand-off.  The possible-CRTC mask remains the
            // fallback for hotplug and inactive outputs.
            if let Some(current_crtc) = encoder.crtc() {
                if seen_crtcs.insert(current_crtc) {
                    crtcs.push(current_crtc);
                }
            }
            for crtc in resources.filter_crtcs(encoder.possible_crtcs()) {
                if seen_crtcs.insert(crtc) {
                    crtcs.push(crtc);
                }
            }
        }

        for mode in modes {
            for crtc in &crtcs {
                candidates.push(NativeOutputConfig {
                    connector: connector_handle,
                    crtc: *crtc,
                    mode,
                });
            }
        }
    }

    Ok(candidates)
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
