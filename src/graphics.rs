//! Pure graphics-backend selection for Rouch.
//!
//! Vulkan is the preferred renderer, but the compositor must remain usable on
//! machines where the Vulkan loader or driver is missing.  This module does
//! not probe the machine and does not create a graphics context.  A platform
//! backend reports a [`BackendProbe`] and this module turns those reports into
//! a deterministic decision, diagnostics, and a bounded performance policy.
//!
//! The third path is deliberately boring: CPU/software rendering with opaque
//! shell material.  It is a usable last resort, not a claim that every
//! platform has a native software compositor today.  In particular, the
//! current Smithay/Winit nested path still constructs a GLES/OpenGL renderer;
//! Vulkan becomes real only when a platform integration supplies a Vulkan
//! probe and renderer.

use std::{fmt, str::FromStr};

/// Graphics APIs understood by the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GraphicsBackend {
    /// The preferred backend.  It is tried before OpenGL by default.
    #[default]
    Vulkan,
    /// The compatibility backend used when Vulkan is unavailable.
    OpenGl,
    /// CPU/raster fallback.  It must use opaque material and bounded motion.
    Software,
}

/// Short alias useful to renderer integrations that call the value a
/// renderer rather than a backend.
pub type RendererBackend = GraphicsBackend;

impl GraphicsBackend {
    /// Compatibility constant for call sites that use the API's spelling.
    pub const OPENGL: Self = Self::OpenGl;

    /// Return every backend in the canonical preference order.
    pub const fn all() -> [Self; 3] {
        [Self::Vulkan, Self::OpenGl, Self::Software]
    }

    /// Whether this backend needs an accelerated graphics device.
    pub const fn is_accelerated(self) -> bool {
        !matches!(self, Self::Software)
    }

    /// Whether the backend must use the opaque visual recipe.
    pub const fn is_opaque(self) -> bool {
        matches!(self, Self::Software)
    }

    /// Human-readable name suitable for logs and diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
            Self::Software => "Software/opaque",
        }
    }
}

impl fmt::Display for GraphicsBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl FromStr for GraphicsBackend {
    type Err = GraphicsBackendParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "vulkan" | "vk" => Ok(Self::Vulkan),
            "opengl" | "open-gl" | "gl" => Ok(Self::OpenGl),
            "software" | "sw" | "cpu" | "opaque" => Ok(Self::Software),
            _ => Err(GraphicsBackendParseError {
                value: value.to_owned(),
            }),
        }
    }
}

/// Error returned when a backend name cannot be parsed from configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsBackendParseError {
    /// The original value supplied by the caller.
    pub value: String,
}

impl fmt::Display for GraphicsBackendParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown graphics backend {:?}", self.value)
    }
}

impl std::error::Error for GraphicsBackendParseError {}

/// State of one backend during discovery and activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BackendState {
    /// No probe has been started yet.
    #[default]
    Unprobed,
    /// A platform backend is currently checking the loader and driver.
    Probing,
    /// The backend passed its probe and can be activated.
    Ready,
    /// The backend was chosen for this compositor instance.
    Active,
    /// The selected device/context disappeared and must not be reused.
    ContextLost,
    /// A lost or failed backend is being probed again for recovery.
    Recovering,
    /// The probe or initialization failed.
    Failed,
}

/// Alias for integrations that use “graphics state” terminology.
pub type GraphicsState = BackendState;

/// Stable reason codes used in diagnostics and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    /// The selector has not received a probe yet.
    NotProbed,
    /// A probe started.
    ProbeStarted,
    /// The driver and loader are usable.
    Available,
    /// No compatible loader/driver was found.
    MissingDriver,
    /// The device or required feature is not supported.
    Unsupported,
    /// Context/device initialization failed.
    InitializationFailed,
    /// A backend failed after it had been selected.
    RuntimeFailure,
    /// A selected device/context was lost while running.
    ContextLost,
    /// A recovery probe started after context loss.
    RecoveryStarted,
    /// The backend was selected without using a fallback.
    Selected,
    /// The backend was selected after the preferred backend failed.
    FallbackSelected,
    /// The software/opaque path was selected as the final safety net.
    OpaqueFallback,
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::NotProbed => "not-probed",
            Self::ProbeStarted => "probe-started",
            Self::Available => "available",
            Self::MissingDriver => "missing-driver",
            Self::Unsupported => "unsupported",
            Self::InitializationFailed => "initialization-failed",
            Self::RuntimeFailure => "runtime-failure",
            Self::ContextLost => "context-lost",
            Self::RecoveryStarted => "recovery-started",
            Self::Selected => "selected",
            Self::FallbackSelected => "fallback-selected",
            Self::OpaqueFallback => "opaque-fallback",
        };
        formatter.write_str(name)
    }
}

/// A structured explanation of the last known state of a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendDiagnostic {
    /// Backend to which this diagnostic belongs.
    pub backend: GraphicsBackend,
    /// State represented by the diagnostic.
    pub state: BackendState,
    /// Machine-readable reason.
    pub code: DiagnosticCode,
    /// Human-readable explanation, suitable for a log or settings panel.
    pub message: String,
    /// Optional renderer/device detail reported by the platform probe.
    pub detail: Option<String>,
}

impl BackendDiagnostic {
    fn new(
        backend: GraphicsBackend,
        state: BackendState,
        code: DiagnosticCode,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            backend,
            state,
            code,
            message: message.into(),
            detail,
        }
    }
}

/// Result supplied by a platform-specific loader/context probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendProbe {
    /// The backend can create the context.  `renderer` may identify the GPU.
    Available { renderer: Option<String> },
    /// The loader or driver is not present.
    MissingDriver { message: String },
    /// The driver exists but cannot satisfy Rouch's requirements.
    Unsupported { message: String },
    /// Context/device creation failed for another reason.
    InitializationFailed { message: String },
}

impl BackendProbe {
    /// Construct a successful probe without a renderer name.
    pub const fn available() -> Self {
        Self::Available { renderer: None }
    }

    /// Construct a successful probe carrying a renderer/device name.
    pub fn available_on(renderer: impl Into<String>) -> Self {
        Self::Available {
            renderer: Some(renderer.into()),
        }
    }

    /// Construct a missing-driver result.
    pub fn missing_driver(message: impl Into<String>) -> Self {
        Self::MissingDriver {
            message: message.into(),
        }
    }

    /// Construct an unsupported-device result.
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
        }
    }

    /// Construct an initialization failure.
    pub fn initialization_failed(message: impl Into<String>) -> Self {
        Self::InitializationFailed {
            message: message.into(),
        }
    }
}

/// How aggressively Rouch should spend GPU and battery budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PerformanceMode {
    /// Keep visual effects enabled and do not impose a frame-rate cap.
    Quality,
    /// The default compromise for a desktop session.
    #[default]
    Balanced,
    /// Prefer low latency and predictable frame pacing over effects.
    Performance,
    /// Reduce work on battery-powered machines.
    BatterySaver,
}

/// Bounded rendering decisions shared by Vulkan and OpenGL integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerformancePolicy {
    /// Mode from which this policy was derived.
    pub mode: PerformanceMode,
    /// Optional compositor frame-rate ceiling.
    pub target_fps: Option<u32>,
    /// Maximum number of frames submitted before the compositor waits.
    pub max_frames_in_flight: u8,
    /// Maximum amount of frame latency the renderer should allow.
    pub max_frame_latency: u8,
    /// Whether shell backdrop blur may be requested.
    pub enable_blur: bool,
    /// Whether decorative shadows may be drawn.
    pub enable_shadows: bool,
    /// Whether non-essential shell animations may run.
    pub enable_animations: bool,
    /// Whether damage tracking should be used to avoid full-screen redraws.
    pub enable_damage_tracking: bool,
    /// Maximum device scale expressed as thousandths (1500 = 1.5x).
    pub max_dpr_milli: u16,
    /// Maximum sampled backdrop pixels for one frame.
    pub max_backdrop_pixels: u32,
    /// Maximum number of concurrent shell surfaces allowed to sample glass.
    pub max_translucent_surfaces: u8,
    /// Approximate cap for reusable backdrop resources.
    pub max_cached_backdrop_bytes: u32,
    /// Maximum duration of a spatial transition in milliseconds.
    pub max_animation_ms: u16,
    /// Prefer compositor-paced redraw over an unbounded render loop.
    pub prefer_vsync: bool,
}

impl PerformancePolicy {
    /// The hard safety policy used by the CPU/opaque renderer.
    pub const fn software_opaque() -> Self {
        Self {
            mode: PerformanceMode::BatterySaver,
            target_fps: Some(30),
            max_frames_in_flight: 1,
            max_frame_latency: 1,
            enable_blur: false,
            enable_shadows: false,
            enable_animations: false,
            enable_damage_tracking: true,
            max_dpr_milli: 1000,
            max_backdrop_pixels: 0,
            max_translucent_surfaces: 0,
            max_cached_backdrop_bytes: 0,
            max_animation_ms: 0,
            prefer_vsync: true,
        }
    }

    /// Return a conservative, deterministic policy for a mode.
    pub const fn for_mode(mode: PerformanceMode) -> Self {
        match mode {
            PerformanceMode::Quality => Self {
                mode,
                target_fps: None,
                max_frames_in_flight: 3,
                max_frame_latency: 2,
                enable_blur: true,
                enable_shadows: true,
                enable_animations: true,
                enable_damage_tracking: true,
                max_dpr_milli: 2000,
                max_backdrop_pixels: 4_194_304,
                max_translucent_surfaces: 4,
                max_cached_backdrop_bytes: 16_777_216,
                max_animation_ms: 700,
                prefer_vsync: true,
            },
            PerformanceMode::Balanced => Self {
                mode,
                target_fps: Some(60),
                max_frames_in_flight: 2,
                max_frame_latency: 2,
                enable_blur: true,
                enable_shadows: true,
                enable_animations: true,
                enable_damage_tracking: true,
                max_dpr_milli: 1500,
                max_backdrop_pixels: 1_048_576,
                max_translucent_surfaces: 3,
                max_cached_backdrop_bytes: 8_388_608,
                max_animation_ms: 360,
                prefer_vsync: true,
            },
            PerformanceMode::Performance => Self {
                mode,
                target_fps: Some(120),
                max_frames_in_flight: 2,
                max_frame_latency: 1,
                enable_blur: false,
                enable_shadows: false,
                enable_animations: true,
                enable_damage_tracking: true,
                max_dpr_milli: 1250,
                max_backdrop_pixels: 0,
                max_translucent_surfaces: 1,
                max_cached_backdrop_bytes: 0,
                max_animation_ms: 260,
                prefer_vsync: true,
            },
            PerformanceMode::BatterySaver => Self {
                mode,
                target_fps: Some(30),
                max_frames_in_flight: 1,
                max_frame_latency: 1,
                enable_blur: false,
                enable_shadows: false,
                enable_animations: false,
                enable_damage_tracking: true,
                max_dpr_milli: 1000,
                max_backdrop_pixels: 0,
                max_translucent_surfaces: 0,
                max_cached_backdrop_bytes: 0,
                max_animation_ms: 0,
                prefer_vsync: true,
            },
        }
    }
}

impl Default for PerformancePolicy {
    fn default() -> Self {
        Self::for_mode(PerformanceMode::Balanced)
    }
}

/// Selection preferences.  Vulkan-first is intentional and explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsConfig {
    /// Backend tried first.
    pub preferred_backend: GraphicsBackend,
    /// Whether the selector may use the other backend after a failure.
    pub allow_fallback: bool,
    /// Rendering policy independent of the selected API.
    pub performance_mode: PerformanceMode,
}

impl GraphicsConfig {
    /// Return the preferred backend followed by the accelerated and CPU
    /// fallbacks.  The default order is always Vulkan -> OpenGL -> software.
    pub const fn candidate_order(self) -> [GraphicsBackend; 3] {
        match self.preferred_backend {
            GraphicsBackend::Vulkan => [
                GraphicsBackend::Vulkan,
                GraphicsBackend::OpenGl,
                GraphicsBackend::Software,
            ],
            GraphicsBackend::OpenGl => [
                GraphicsBackend::OpenGl,
                GraphicsBackend::Vulkan,
                GraphicsBackend::Software,
            ],
            GraphicsBackend::Software => [
                GraphicsBackend::Software,
                GraphicsBackend::Vulkan,
                GraphicsBackend::OpenGl,
            ],
        }
    }

    /// Resolve the performance mode into the concrete policy.
    pub const fn performance_policy(self) -> PerformancePolicy {
        PerformancePolicy::for_mode(self.performance_mode)
    }
}

impl Default for GraphicsConfig {
    fn default() -> Self {
        Self {
            preferred_backend: GraphicsBackend::Vulkan,
            allow_fallback: true,
            performance_mode: PerformanceMode::Balanced,
        }
    }
}

/// Overall result of a graphics selection attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionState {
    /// At least one backend still needs to be probed.
    Pending,
    /// A backend was selected and can be initialized by the platform layer.
    Ready,
    /// Every permitted candidate failed.
    Failed,
}

/// Deterministic backend choice plus all diagnostics collected along the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsDecision {
    /// State of this selection attempt.
    pub state: SelectionState,
    /// Selected backend, if one is ready.
    pub backend: Option<GraphicsBackend>,
    /// True when the selected backend is not the configured preference.
    pub fallback_used: bool,
    /// True when the final software/opaque safety path is active.
    pub opaque_only: bool,
    /// Policy to pass to the renderer.
    pub performance: PerformancePolicy,
    /// One diagnostic per candidate, in candidate order.
    pub diagnostics: Vec<BackendDiagnostic>,
}

impl GraphicsDecision {
    fn pending(config: GraphicsConfig, diagnostics: Vec<BackendDiagnostic>) -> Self {
        Self {
            state: SelectionState::Pending,
            backend: None,
            fallback_used: false,
            opaque_only: false,
            performance: config.performance_policy(),
            diagnostics,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BackendRecord {
    state: BackendState,
    diagnostic: BackendDiagnostic,
}

/// Error returned when the explicit probe state machine is used incorrectly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphicsSelectionError {
    /// The caller attempted an invalid state transition.
    InvalidTransition {
        backend: GraphicsBackend,
        from: BackendState,
        expected: &'static str,
    },
    /// A backend was reported after another backend had already been chosen.
    AlreadySelected { backend: GraphicsBackend },
}

impl fmt::Display for GraphicsSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition {
                backend,
                from,
                expected,
            } => write!(
                formatter,
                "cannot transition {backend} from {from:?}; expected {expected}"
            ),
            Self::AlreadySelected { backend } => write!(formatter, "{backend} is already selected"),
        }
    }
}

impl std::error::Error for GraphicsSelectionError {}

/// Stateful selector used by a compositor startup sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsSelector {
    config: GraphicsConfig,
    records: [BackendRecord; 3],
    active: Option<GraphicsBackend>,
}

impl GraphicsSelector {
    /// Start a Vulkan-first selection with all candidates unprobed.
    pub fn new(config: GraphicsConfig) -> Self {
        let order = config.candidate_order();
        Self {
            config,
            records: [
                Self::record_for(order[0]),
                Self::record_for(order[1]),
                Self::record_for(order[2]),
            ],
            active: None,
        }
    }

    fn record_for(backend: GraphicsBackend) -> BackendRecord {
        BackendRecord {
            state: BackendState::Unprobed,
            diagnostic: BackendDiagnostic::new(
                backend,
                BackendState::Unprobed,
                DiagnosticCode::NotProbed,
                format!("{} has not been probed", backend.name()),
                None,
            ),
        }
    }

    /// Current configuration.
    pub const fn config(&self) -> GraphicsConfig {
        self.config
    }

    /// Current state of one backend.
    pub fn backend_state(&self, backend: GraphicsBackend) -> BackendState {
        self.record(backend).state
    }

    /// All current diagnostics in candidate order.
    pub fn diagnostics(&self) -> Vec<BackendDiagnostic> {
        self.records
            .iter()
            .map(|record| record.diagnostic.clone())
            .collect()
    }

    fn record_index(&self, backend: GraphicsBackend) -> usize {
        self.config
            .candidate_order()
            .iter()
            .position(|candidate| *candidate == backend)
            .expect("candidate_order always contains every graphics backend")
    }

    fn record(&self, backend: GraphicsBackend) -> &BackendRecord {
        &self.records[self.record_index(backend)]
    }

    fn record_mut(&mut self, backend: GraphicsBackend) -> &mut BackendRecord {
        let index = self.record_index(backend);
        &mut self.records[index]
    }

    /// Mark a backend as being probed.
    pub fn begin_probe(&mut self, backend: GraphicsBackend) -> Result<(), GraphicsSelectionError> {
        if self.active == Some(backend) {
            return Err(GraphicsSelectionError::AlreadySelected { backend });
        }

        let current = self.backend_state(backend);
        if !matches!(current, BackendState::Unprobed | BackendState::Failed) {
            return Err(GraphicsSelectionError::InvalidTransition {
                backend,
                from: current,
                expected: "Unprobed or Failed",
            });
        }

        let record = self.record_mut(backend);
        record.state = BackendState::Probing;
        record.diagnostic = BackendDiagnostic::new(
            backend,
            BackendState::Probing,
            DiagnosticCode::ProbeStarted,
            format!("probing {}", backend.name()),
            None,
        );
        Ok(())
    }

    /// Start a probe specifically to recover a backend whose context was lost.
    /// Other backends may remain active while this happens, so recovery can be
    /// attempted without taking down the usable fallback desktop.
    pub fn begin_recovery(&mut self, backend: GraphicsBackend) -> Result<(), GraphicsSelectionError> {
        if self.active == Some(backend) {
            return Err(GraphicsSelectionError::AlreadySelected { backend });
        }

        let current = self.backend_state(backend);
        if !matches!(current, BackendState::ContextLost | BackendState::Failed) {
            return Err(GraphicsSelectionError::InvalidTransition {
                backend,
                from: current,
                expected: "ContextLost or Failed",
            });
        }

        let record = self.record_mut(backend);
        record.state = BackendState::Recovering;
        record.diagnostic = BackendDiagnostic::new(
            backend,
            BackendState::Recovering,
            DiagnosticCode::RecoveryStarted,
            format!("recovering {} after a context failure", backend.name()),
            None,
        );
        Ok(())
    }

    /// Finish a probe and retain a structured diagnostic.
    pub fn record_probe(
        &mut self,
        backend: GraphicsBackend,
        probe: BackendProbe,
    ) -> Result<(), GraphicsSelectionError> {
        if self.active == Some(backend) {
            return Err(GraphicsSelectionError::AlreadySelected { backend });
        }

        let current = self.backend_state(backend);
        if !matches!(
            current,
            BackendState::Unprobed
                | BackendState::Probing
                | BackendState::Failed
                | BackendState::ContextLost
                | BackendState::Recovering
        ) {
            return Err(GraphicsSelectionError::InvalidTransition {
                backend,
                from: current,
                expected: "Unprobed, Probing, Failed, ContextLost, or Recovering",
            });
        }

        let (state, code, message, detail) = match probe {
            BackendProbe::Available { renderer } => (
                BackendState::Ready,
                DiagnosticCode::Available,
                format!("{} is available", backend.name()),
                renderer,
            ),
            BackendProbe::MissingDriver { message } => {
                (BackendState::Failed, DiagnosticCode::MissingDriver, message, None)
            }
            BackendProbe::Unsupported { message } => {
                (BackendState::Failed, DiagnosticCode::Unsupported, message, None)
            }
            BackendProbe::InitializationFailed { message } => (
                BackendState::Failed,
                DiagnosticCode::InitializationFailed,
                message,
                None,
            ),
        };

        let record = self.record_mut(backend);
        record.state = state;
        record.diagnostic = BackendDiagnostic::new(backend, state, code, message, detail);
        Ok(())
    }

    /// Record that the active device/context disappeared.  The failed path is
    /// kept in diagnostics and selection is immediately allowed to choose the
    /// next ready backend.
    pub fn mark_context_loss(
        &mut self,
        message: impl Into<String>,
    ) -> Result<GraphicsBackend, GraphicsSelectionError> {
        let Some(backend) = self.active.take() else {
            return Err(GraphicsSelectionError::InvalidTransition {
                backend: self.config.preferred_backend,
                from: BackendState::Unprobed,
                expected: "Active",
            });
        };

        let detail = self.record(backend).diagnostic.detail.clone();
        let record = self.record_mut(backend);
        record.state = BackendState::ContextLost;
        record.diagnostic = BackendDiagnostic::new(
            backend,
            BackendState::ContextLost,
            DiagnosticCode::ContextLost,
            message,
            detail,
        );
        Ok(backend)
    }

    /// Mark the active backend as failed at runtime so the caller can decide
    /// whether to restart selection with the remaining backend.
    pub fn mark_runtime_failure(&mut self, message: impl Into<String>) -> Result<(), GraphicsSelectionError> {
        let Some(backend) = self.active.take() else {
            return Err(GraphicsSelectionError::InvalidTransition {
                backend: self.config.preferred_backend,
                from: BackendState::Unprobed,
                expected: "Active",
            });
        };
        let record = self.record_mut(backend);
        record.state = BackendState::Failed;
        record.diagnostic = BackendDiagnostic::new(
            backend,
            BackendState::Failed,
            DiagnosticCode::RuntimeFailure,
            message,
            None,
        );
        Ok(())
    }

    /// Choose the first ready/active candidate in preference order.
    pub fn select(&mut self) -> GraphicsDecision {
        let order = self.config.candidate_order();
        let chosen = order
            .iter()
            .enumerate()
            .filter(|(index, _)| *index == 0 || self.config.allow_fallback)
            .find(|(_, backend)| {
                matches!(
                    self.backend_state(**backend),
                    BackendState::Ready | BackendState::Active
                )
            })
            .map(|(_, backend)| *backend);

        if let Some(chosen) = chosen {
            let previous = self.active;
            if previous != Some(chosen) {
                if let Some(previous) = previous {
                    let previous_detail = self.record(previous).diagnostic.detail.clone();
                    let record = self.record_mut(previous);
                    record.state = BackendState::Ready;
                    record.diagnostic = BackendDiagnostic::new(
                        previous,
                        BackendState::Ready,
                        DiagnosticCode::Available,
                        format!("{} kept ready after renderer handoff", previous.name()),
                        previous_detail,
                    );
                }

                let preferred_backend = self.config.preferred_backend;
                let fallback_used = chosen != preferred_backend;
                let detail = self.record(chosen).diagnostic.detail.clone();
                let (code, message) = if chosen.is_opaque() {
                    (
                        DiagnosticCode::OpaqueFallback,
                        format!(
                            "{} selected as the final opaque fallback after accelerated paths failed",
                            chosen.name()
                        ),
                    )
                } else if fallback_used {
                    (
                        DiagnosticCode::FallbackSelected,
                        format!(
                            "{} selected as fallback after {} failed",
                            chosen.name(),
                            preferred_backend.name()
                        ),
                    )
                } else {
                    (DiagnosticCode::Selected, format!("{} selected", chosen.name()))
                };
                let record = self.record_mut(chosen);
                record.state = BackendState::Active;
                record.diagnostic =
                    BackendDiagnostic::new(chosen, BackendState::Active, code, message, detail);
                self.active = Some(chosen);
            }

            let fallback_used = chosen != self.config.preferred_backend;
            return GraphicsDecision {
                state: SelectionState::Ready,
                backend: Some(chosen),
                fallback_used,
                opaque_only: chosen.is_opaque(),
                performance: if chosen.is_opaque() {
                    PerformancePolicy::software_opaque()
                } else {
                    self.config.performance_policy()
                },
                diagnostics: self.diagnostics(),
            };
        }

        let has_unprobed = self.records.iter().any(|record| {
            matches!(
                record.state,
                BackendState::Unprobed | BackendState::Probing | BackendState::Recovering
            )
        });
        if has_unprobed {
            GraphicsDecision::pending(self.config, self.diagnostics())
        } else {
            GraphicsDecision {
                state: SelectionState::Failed,
                backend: None,
                fallback_used: false,
                opaque_only: false,
                performance: self.config.performance_policy(),
                diagnostics: self.diagnostics(),
            }
        }
    }
}

/// Select a backend from two accelerated probe results and an always-usable
/// software/opaque safety path.  This keeps the original two-probe API useful
/// to integrations while making the final fallback explicit in diagnostics.
pub fn select_backend(
    config: GraphicsConfig,
    vulkan: BackendProbe,
    opengl: BackendProbe,
) -> GraphicsDecision {
    select_backend_with_software(
        config,
        vulkan,
        opengl,
        BackendProbe::available_on("CPU raster / opaque shell"),
    )
}

/// Select a backend from complete probes for every supported path.
pub fn select_backend_with_software(
    config: GraphicsConfig,
    vulkan: BackendProbe,
    opengl: BackendProbe,
    software: BackendProbe,
) -> GraphicsDecision {
    let mut selector = GraphicsSelector::new(config);
    let order = config.candidate_order();
    let probes = [
        (GraphicsBackend::Vulkan, vulkan),
        (GraphicsBackend::OpenGl, opengl),
        (GraphicsBackend::Software, software),
    ];

    for candidate in order {
        let probe = probes
            .iter()
            .find(|(backend, _)| *backend == candidate)
            .map(|(_, probe)| probe.clone())
            .expect("the probe array contains both backends");
        let _ = selector.record_probe(candidate, probe);
    }

    selector.select()
}

/// Convenience helper for callers that only have boolean availability.
pub fn select_from_availability(vulkan_available: bool, opengl_available: bool) -> GraphicsDecision {
    select_backend(
        GraphicsConfig::default(),
        if vulkan_available {
            BackendProbe::available()
        } else {
            BackendProbe::missing_driver("Vulkan loader or driver is unavailable")
        },
        if opengl_available {
            BackendProbe::available()
        } else {
            BackendProbe::missing_driver("OpenGL/EGL driver is unavailable")
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_vulkan_and_falls_back_to_opengl() {
        let decision = select_from_availability(false, true);

        assert_eq!(decision.state, SelectionState::Ready);
        assert_eq!(decision.backend, Some(GraphicsBackend::OpenGl));
        assert!(decision.fallback_used);
        assert_eq!(decision.diagnostics[0].code, DiagnosticCode::MissingDriver);
        assert_eq!(decision.diagnostics[1].code, DiagnosticCode::FallbackSelected);
    }

    #[test]
    fn vulkan_wins_when_both_backends_are_available() {
        let decision = select_from_availability(true, true);

        assert_eq!(decision.backend, Some(GraphicsBackend::Vulkan));
        assert!(!decision.fallback_used);
        assert_eq!(decision.diagnostics[0].state, BackendState::Active);
    }

    #[test]
    fn selector_waits_until_probes_are_complete() {
        let mut selector = GraphicsSelector::new(GraphicsConfig::default());

        assert_eq!(selector.select().state, SelectionState::Pending);
        selector.begin_probe(GraphicsBackend::Vulkan).unwrap();
        assert_eq!(
            selector.backend_state(GraphicsBackend::Vulkan),
            BackendState::Probing
        );
        assert_eq!(selector.select().state, SelectionState::Pending);
        selector
            .record_probe(GraphicsBackend::Vulkan, BackendProbe::missing_driver("no loader"))
            .unwrap();
        assert_eq!(selector.select().state, SelectionState::Pending);
        selector.begin_probe(GraphicsBackend::OpenGl).unwrap();
        selector
            .record_probe(
                GraphicsBackend::OpenGl,
                BackendProbe::available_on("Mesa llvmpipe"),
            )
            .unwrap();

        let decision = selector.select();
        assert_eq!(decision.backend, Some(GraphicsBackend::OpenGl));
        assert_eq!(decision.diagnostics[1].detail.as_deref(), Some("Mesa llvmpipe"));
    }

    #[test]
    fn fallback_can_be_disabled() {
        let config = GraphicsConfig {
            allow_fallback: false,
            ..GraphicsConfig::default()
        };
        let decision = select_backend(
            config,
            BackendProbe::missing_driver("no Vulkan"),
            BackendProbe::available(),
        );

        assert_eq!(decision.state, SelectionState::Failed);
        assert_eq!(decision.backend, None);
    }

    #[test]
    fn performance_policy_bounds_expensive_effects() {
        let balanced = PerformancePolicy::for_mode(PerformanceMode::Balanced);
        let saver = PerformancePolicy::for_mode(PerformanceMode::BatterySaver);

        assert!(balanced.enable_blur);
        assert!(!saver.enable_blur);
        assert!(!saver.enable_shadows);
        assert_eq!(saver.target_fps, Some(30));
        assert!(saver.max_frames_in_flight < balanced.max_frames_in_flight);
        assert!(balanced.max_backdrop_pixels > saver.max_backdrop_pixels);
        assert!(balanced.max_dpr_milli > saver.max_dpr_milli);
        assert!(balanced.prefer_vsync && saver.prefer_vsync);
    }

    #[test]
    fn software_is_the_last_opaque_safety_net() {
        let decision = select_from_availability(false, false);

        assert_eq!(decision.state, SelectionState::Ready);
        assert_eq!(decision.backend, Some(GraphicsBackend::Software));
        assert!(decision.fallback_used);
        assert!(decision.opaque_only);
        assert_eq!(decision.diagnostics[2].code, DiagnosticCode::OpaqueFallback);
        assert!(!decision.performance.enable_blur);
        assert_eq!(decision.performance.max_backdrop_pixels, 0);
    }

    #[test]
    fn context_loss_falls_back_and_can_recover_preferred_backend() {
        let mut selector = GraphicsSelector::new(GraphicsConfig::default());
        selector
            .record_probe(
                GraphicsBackend::Vulkan,
                BackendProbe::available_on("test Vulkan device"),
            )
            .unwrap();
        selector
            .record_probe(
                GraphicsBackend::OpenGl,
                BackendProbe::available_on("test OpenGL device"),
            )
            .unwrap();
        selector
            .record_probe(
                GraphicsBackend::Software,
                BackendProbe::available_on("test CPU raster"),
            )
            .unwrap();

        assert_eq!(selector.select().backend, Some(GraphicsBackend::Vulkan));
        assert_eq!(
            selector.mark_context_loss("simulated device reset").unwrap(),
            GraphicsBackend::Vulkan
        );
        assert_eq!(
            selector.backend_state(GraphicsBackend::Vulkan),
            BackendState::ContextLost
        );
        assert_eq!(selector.select().backend, Some(GraphicsBackend::OpenGl));

        selector.begin_recovery(GraphicsBackend::Vulkan).unwrap();
        assert_eq!(
            selector.backend_state(GraphicsBackend::Vulkan),
            BackendState::Recovering
        );
        selector
            .record_probe(
                GraphicsBackend::Vulkan,
                BackendProbe::available_on("recovered Vulkan device"),
            )
            .unwrap();
        assert_eq!(selector.select().backend, Some(GraphicsBackend::Vulkan));
        assert_eq!(
            selector.backend_state(GraphicsBackend::OpenGl),
            BackendState::Ready
        );
    }

    #[test]
    fn explicit_software_probe_can_fail_and_reports_degraded_state() {
        let decision = select_backend_with_software(
            GraphicsConfig::default(),
            BackendProbe::missing_driver("no Vulkan loader"),
            BackendProbe::missing_driver("no OpenGL/EGL driver"),
            BackendProbe::unsupported("CPU raster path unavailable"),
        );

        assert_eq!(decision.state, SelectionState::Failed);
        assert_eq!(decision.backend, None);
        assert_eq!(decision.diagnostics[2].code, DiagnosticCode::Unsupported);
    }

    #[test]
    fn backend_names_parse_case_insensitively() {
        assert_eq!("vk".parse::<GraphicsBackend>(), Ok(GraphicsBackend::Vulkan));
        assert_eq!("OpenGL".parse::<GraphicsBackend>(), Ok(GraphicsBackend::OpenGl));
        assert_eq!("opaque".parse::<GraphicsBackend>(), Ok(GraphicsBackend::Software));
        assert!("metal".parse::<GraphicsBackend>().is_err());
    }
}
