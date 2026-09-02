//! Explicit startup/session policy shared by the native and nested backends.
//!
//! This module deliberately contains no Wayland, DRM, or process side effects.
//! It only interprets command-line/session metadata.  The native backend is the
//! only place allowed to acquire a seat or request a VT change.

use std::{env, fmt, path::PathBuf};

/// The graphical backend Rouch should start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// Run inside the current Wayland/X11 session through Smithay's Winit path.
    Nested,
    /// Attempt to own a native Linux seat and DRM output.
    Native,
}

impl fmt::Display for SessionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Nested => "nested",
            Self::Native => "session",
        })
    }
}

/// Command-line choices that affect startup ownership and recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOptions {
    pub mode: SessionMode,
    /// Whether a failed native startup may return to the safe nested backend.
    pub fallback_nested: bool,
    /// Optional seat override. Normally this comes from `XDG_SEAT`.
    pub seat: Option<String>,
    /// An explicit VT to request through libseat after the seat is acquired.
    pub vt: Option<i32>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            // Keep existing development behavior unchanged when no switch is
            // supplied. Native ownership must always be explicit.
            mode: SessionMode::Nested,
            fallback_nested: true,
            seat: None,
            vt: None,
        }
    }
}

/// Parse only Rouch's startup flags. Unknown flags are rejected so a typo can
/// never silently turn a native session request into another mode.
pub fn parse_args(args: &[String]) -> Result<SessionOptions, String> {
    let mut options = SessionOptions::default();
    let mut selected_mode: Option<SessionMode> = None;

    for arg in args {
        match arg.as_str() {
            "--nested" => select_mode(&mut selected_mode, SessionMode::Nested)?,
            "--session" => select_mode(&mut selected_mode, SessionMode::Native)?,
            "--no-fallback" => options.fallback_nested = false,
            "--fallback-nested" => options.fallback_nested = true,
            "--help" | "-h" => {
                return Err(usage().to_owned());
            }
            value if value.starts_with("--seat=") => {
                let seat = value.trim_start_matches("--seat=");
                if seat.is_empty() || !valid_seat_name(seat) {
                    return Err(format!("invalid seat name in {value:?}"));
                }
                options.seat = Some(seat.to_owned());
            }
            value if value.starts_with("--vt=") => {
                let raw = value.trim_start_matches("--vt=");
                let vt = raw
                    .parse::<i32>()
                    .map_err(|_| format!("invalid VT number in {value:?}"))?;
                options.vt = Some(validate_vt(vt)?);
            }
            value => return Err(format!("unknown Rouch option {value:?}\n\n{}", usage())),
        }
    }

    options.mode = selected_mode.unwrap_or(options.mode);
    Ok(options)
}

fn select_mode(selected: &mut Option<SessionMode>, requested: SessionMode) -> Result<(), String> {
    if let Some(previous) = *selected {
        if previous != requested {
            return Err(format!(
                "conflicting session modes: --{previous} and --{requested}"
            ));
        }
    } else {
        *selected = Some(requested);
    }
    Ok(())
}

fn valid_seat_name(seat: &str) -> bool {
    seat.len() <= 64
        && !seat.is_empty()
        && seat
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Validate the VT range accepted by the native adapter.
///
/// Linux currently exposes VTs 1..=63. libseat performs the final backend-
/// specific validation; keeping the range here prevents accidental values such
/// as zero or a negative ioctl/session number from reaching it.
pub fn validate_vt(vt: i32) -> Result<i32, String> {
    if (1..=63).contains(&vt) {
        Ok(vt)
    } else {
        Err(format!("VT must be between 1 and 63, got {vt}"))
    }
}

/// A small, non-secret snapshot of the XDG/session variables relevant to a
/// compositor startup decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnvironment {
    pub session_type: Option<String>,
    pub session_class: Option<String>,
    pub session_id: Option<String>,
    pub seat: String,
    pub vt: Option<i32>,
    pub runtime_dir: Option<PathBuf>,
    pub wayland_display: Option<String>,
    pub x_display: Option<String>,
    pub current_desktop: Option<String>,
}

impl SessionEnvironment {
    /// Read the process environment without modifying it.
    pub fn from_process() -> Self {
        Self {
            session_type: env_string("XDG_SESSION_TYPE"),
            session_class: env_string("XDG_SESSION_CLASS"),
            session_id: env_string("XDG_SESSION_ID"),
            seat: env_string("XDG_SEAT").unwrap_or_else(|| "seat0".to_owned()),
            vt: env_string("XDG_VTNR").and_then(|value| value.parse().ok()),
            runtime_dir: env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            wayland_display: env_string("WAYLAND_DISPLAY"),
            x_display: env_string("DISPLAY"),
            current_desktop: env_string("XDG_CURRENT_DESKTOP"),
        }
    }

    /// Construct a deterministic snapshot for pure tests and diagnostics.
    pub fn for_test(
        session_type: Option<&str>,
        vt: Option<i32>,
        wayland_display: Option<&str>,
        x_display: Option<&str>,
    ) -> Self {
        Self {
            session_type: session_type.map(str::to_owned),
            session_class: None,
            session_id: None,
            seat: "seat0".to_owned(),
            vt,
            runtime_dir: Some(PathBuf::from("/run/user/test")),
            wayland_display: wayland_display.map(str::to_owned),
            x_display: x_display.map(str::to_owned),
            current_desktop: None,
        }
    }

    /// Whether another graphical server is already using this process session.
    pub fn has_graphical_host(&self) -> bool {
        self.wayland_display.is_some()
            || self.x_display.is_some()
            || matches!(self.session_type.as_deref(), Some("wayland" | "x11"))
    }

    /// Native startup is most predictable from a VT without a host display.
    /// The native adapter still asks libseat for the final authority decision.
    pub fn native_readiness(&self) -> NativeReadiness {
        if self.runtime_dir.is_none() {
            return NativeReadiness::MissingRuntimeDirectory;
        }
        if self.has_graphical_host() {
            return NativeReadiness::ExistingGraphicalHost;
        }
        if self.vt.is_some() || self.session_type.as_deref() == Some("tty") {
            NativeReadiness::TtyCandidate
        } else {
            NativeReadiness::UnknownHost
        }
    }

    /// Human-readable diagnostics safe to print in a startup error.
    pub fn diagnostic(&self) -> String {
        format!(
            "session_type={:?}, class={:?}, id={:?}, seat={}, vt={:?}, runtime_dir={}, wayland={:?}, x11={:?}, desktop={:?}",
            self.session_type,
            self.session_class,
            self.session_id,
            self.seat,
            self.vt,
            self.runtime_dir
                .as_ref()
                .map_or_else(|| "missing".to_owned(), |path| path.display().to_string()),
            self.wayland_display,
            self.x_display,
            self.current_desktop,
        )
    }
}

fn env_string(key: &str) -> Option<String> {
    env::var_os(key).and_then(|value| value.into_string().ok())
}

/// The non-mutating decision made before any native resource is acquired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPlan {
    RunNested,
    TryNative { fallback_nested: bool },
}

pub fn startup_plan(options: &SessionOptions) -> StartupPlan {
    match options.mode {
        SessionMode::Nested => StartupPlan::RunNested,
        SessionMode::Native => StartupPlan::TryNative {
            fallback_nested: options.fallback_nested,
        },
    }
}

/// Native diagnostics that determine whether an explicit VT switch is safe to
/// request. The actual request is performed by `LibSeatSession` in `drm.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VtSwitchPlan {
    NotRequested,
    AlreadyOn(i32),
    Request(i32),
}

pub fn vt_switch_plan(options: &SessionOptions, environment: &SessionEnvironment) -> VtSwitchPlan {
    let Some(requested) = options.vt else {
        return VtSwitchPlan::NotRequested;
    };
    if environment.vt == Some(requested) {
        VtSwitchPlan::AlreadyOn(requested)
    } else {
        VtSwitchPlan::Request(requested)
    }
}

/// Coarse startup context, intentionally kept independent from actual seat
/// acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeReadiness {
    TtyCandidate,
    ExistingGraphicalHost,
    UnknownHost,
    MissingRuntimeDirectory,
}

impl fmt::Display for NativeReadiness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TtyCandidate => "tty candidate",
            Self::ExistingGraphicalHost => "existing graphical host",
            Self::UnknownHost => "unknown host session",
            Self::MissingRuntimeDirectory => "missing XDG_RUNTIME_DIR",
        })
    }
}

/// Text kept in sync with the installer/session desktop entry.
pub const fn usage() -> &'static str {
    "Usage: rouch [--nested|--session] [--no-fallback] [--seat=seat0] [--vt=N]"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn default_stays_nested_for_low_risk_development() {
        let options = parse_args(&[]).unwrap();
        assert_eq!(options.mode, SessionMode::Nested);
        assert!(options.fallback_nested);
        assert_eq!(startup_plan(&options), StartupPlan::RunNested);
    }

    #[test]
    fn native_mode_and_no_fallback_are_explicit() {
        let options = parse_args(&args(&["--session", "--no-fallback", "--seat=seat0", "--vt=2"])).unwrap();
        assert_eq!(options.mode, SessionMode::Native);
        assert!(!options.fallback_nested);
        assert_eq!(options.seat.as_deref(), Some("seat0"));
        assert_eq!(options.vt, Some(2));
        assert_eq!(
            startup_plan(&options),
            StartupPlan::TryNative {
                fallback_nested: false
            }
        );
    }

    #[test]
    fn conflicting_modes_are_rejected() {
        let error = parse_args(&args(&["--nested", "--session"])).unwrap_err();
        assert!(error.contains("conflicting session modes"));
    }

    #[test]
    fn invalid_vt_and_seat_are_rejected_before_libseat() {
        assert!(parse_args(&args(&["--vt=0"])).is_err());
        assert!(parse_args(&args(&["--seat=seat/0"])).is_err());
    }

    #[test]
    fn graphical_host_is_reported_but_not_assumed_to_be_safe_native() {
        let environment = SessionEnvironment::for_test(Some("wayland"), None, Some("wayland-1"), None);
        assert!(environment.has_graphical_host());
        assert_eq!(
            environment.native_readiness(),
            NativeReadiness::ExistingGraphicalHost
        );
    }

    #[test]
    fn tty_candidate_is_selected_without_a_host_socket() {
        let environment = SessionEnvironment::for_test(Some("tty"), Some(2), None, None);
        assert_eq!(environment.native_readiness(), NativeReadiness::TtyCandidate);
    }

    #[test]
    fn vt_plan_distinguishes_already_active_and_requested_switch() {
        let on_vt_two = SessionEnvironment::for_test(Some("tty"), Some(2), None, None);
        let mut options = SessionOptions {
            mode: SessionMode::Native,
            vt: Some(2),
            ..SessionOptions::default()
        };
        assert_eq!(vt_switch_plan(&options, &on_vt_two), VtSwitchPlan::AlreadyOn(2));

        options.vt = Some(3);
        assert_eq!(vt_switch_plan(&options, &on_vt_two), VtSwitchPlan::Request(3));
    }

    #[test]
    fn diagnostics_do_not_require_real_process_environment() {
        let environment = SessionEnvironment::for_test(None, None, None, Some(":0"));
        let diagnostic = environment.diagnostic();
        assert!(diagnostic.contains("x11=Some(\":0\")"));
        assert!(diagnostic.contains("seat=seat0"));
    }
}
