//! Pure game-mode detection and resource policy.
//!
//! This module deliberately does not read `/proc`, talk to cgroups, or change
//! process priorities. A Linux adapter can collect [`GameSnapshot`] values and
//! hand them to [`GameDetector`]. Keeping collection outside this module makes
//! the decision deterministic on every platform, including the Windows test
//! build, and makes every automatic decision explainable.
//!
//! A process name that merely looks like a game is never enough. Automatic
//! activation requires corroborating, observable signals such as a game
//! desktop category, a focused fullscreen window, a Proton/Wine/Sober runtime,
//! a compatible environment key, or a related process tree. Runtime markers
//! are evidence, not a hard-coded catalogue of games.

use std::collections::HashMap;

/// Maximum number of process records consumed by one detection pass.
pub const MAX_PROCESS_SNAPSHOTS: usize = 4096;
/// Maximum number of window records consumed by one detection pass.
pub const MAX_WINDOW_SNAPSHOTS: usize = 1024;
/// Maximum number of desktop entries consumed by one detection pass.
pub const MAX_DESKTOP_ENTRIES: usize = 4096;
/// Maximum number of cgroup records consumed by one detection pass.
pub const MAX_CGROUP_SNAPSHOTS: usize = 4096;
/// Maximum number of distinct reasons returned to a settings/diagnostic UI.
pub const MAX_EVIDENCE_REASONS: usize = 16;

/// Version of the private, temporary shell-state cache format.
pub const CACHE_FORMAT_VERSION: u16 = 1;
/// Hard upper bound for all temporary game-mode cache storage.
pub const MAX_CACHE_BYTES: usize = 4096;
/// Fixed logical cost of one typed shell-state cache entry.
pub const CACHED_SHELL_STATE_BYTES: usize = 128;

/// A process observation supplied by a platform adapter.
///
/// Environment values are intentionally not represented. The adapter should
/// provide names of relevant variables only; this lets detection see
/// `STEAM_COMPAT_DATA_PATH` without copying tokens, passwords, or arbitrary
/// environment contents into the compositor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessSnapshot {
    /// Process identifier, when the adapter could read it.
    pub pid: u32,
    /// Parent process identifier, if known.
    pub parent_pid: Option<u32>,
    /// Executable path or basename.
    pub executable: String,
    /// Argument strings, used only for runtime markers.
    pub argv: Vec<String>,
    /// Names of selected environment variables, never their values.
    pub environment_keys: Vec<String>,
    /// Desktop entry identifier associated by the adapter, if known.
    pub desktop_id: Option<String>,
}

impl ProcessSnapshot {
    /// Construct the smallest useful process observation.
    pub fn new(pid: u32, executable: impl Into<String>) -> Self {
        Self {
            pid,
            executable: executable.into(),
            ..Self::default()
        }
    }
}

/// A window observation supplied by a Wayland/XWayland adapter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowSnapshot {
    /// Stable identifier for this observation, if the adapter has one.
    pub window_id: u64,
    /// Owning process, when the protocol/backend exposes it.
    pub pid: Option<u32>,
    /// Wayland `app_id`, if present.
    pub app_id: Option<String>,
    /// X11 `WM_CLASS`, if present.
    pub wm_class: Option<String>,
    /// Whether the surface currently occupies the output fullscreen.
    pub fullscreen: bool,
    /// Whether the surface is focused according to the adapter.
    pub focused: bool,
}

/// A parsed freedesktop desktop-entry observation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DesktopEntrySnapshot {
    /// Desktop file identifier, such as an installed application id.
    pub desktop_id: String,
    /// Parsed `Categories` values. Values with a trailing `;` are accepted.
    pub categories: Vec<String>,
    /// Matching application id, when the entry declares one.
    pub app_id: Option<String>,
    /// Matching startup WM class, when the entry declares one.
    pub wm_class: Option<String>,
    /// Process associated by an adapter, when available.
    pub pid: Option<u32>,
}

/// A cgroup observation. The path is treated as a signal and is never used as
/// a command target by this module.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CgroupSnapshot {
    /// Process associated with the cgroup, if the adapter can map it.
    pub pid: Option<u32>,
    /// Cgroup path read by the adapter.
    pub path: String,
    /// Controllers present on the cgroup, retained for future policy work.
    pub controllers: Vec<String>,
}

/// A complete point-in-time observation used by the pure detector.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GameSnapshot {
    /// Processes visible to the adapter.
    pub processes: Vec<ProcessSnapshot>,
    /// Windows visible to the adapter.
    pub windows: Vec<WindowSnapshot>,
    /// Installed/parsed desktop entries relevant to the observation.
    pub desktop_entries: Vec<DesktopEntrySnapshot>,
    /// Cgroups relevant to the observed processes.
    pub cgroups: Vec<CgroupSnapshot>,
    /// Foreground process supplied by the session backend, if known.
    pub foreground_pid: Option<u32>,
    /// Explicit user confirmation for this foreground application.
    pub manual_confirmation: bool,
}

/// Runtime family detected from an observable marker.
///
/// This is deliberately a runtime taxonomy, not a list of game titles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeMarker {
    /// Valve's Proton compatibility runtime.
    Proton,
    /// Wine or a Wine-compatible executable.
    Wine,
    /// Steam's pressure-vessel container runtime.
    PressureVessel,
    /// The Sober Linux runtime signal.
    Sober,
    /// A Steam app-id marker without assuming which title it belongs to.
    SteamAppId,
}

/// The type of application inferred from the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GameClassification {
    /// There was not enough evidence to identify an active application.
    #[default]
    Unknown,
    /// An active application was observed, but no game evidence was found.
    CommonApp,
    /// A launcher was observed without gameplay evidence.
    Launcher,
    /// Some independent game evidence exists, but automatic activation is
    /// intentionally not yet justified.
    ProbableGame,
    /// Strong corroborating evidence or explicit user confirmation exists.
    ConfirmedGame,
}

impl GameClassification {
    /// Whether this classification represents a game candidate.
    pub const fn is_game(self) -> bool {
        matches!(self, Self::ProbableGame | Self::ConfirmedGame)
    }

    /// Whether automatic mode may act on this classification.
    pub const fn is_confirmed(self) -> bool {
        matches!(self, Self::ConfirmedGame)
    }

    /// Whether this observation is a launcher-only observation.
    pub const fn is_launcher(self) -> bool {
        matches!(self, Self::Launcher)
    }
}

/// Broad evidence families used to prevent one signal from masquerading as
/// independent corroboration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EvidenceFamily {
    Desktop,
    Window,
    Runtime,
    ProcessTree,
    Cgroup,
    Manual,
    Launcher,
}

impl EvidenceFamily {
    const fn bit(self) -> u8 {
        match self {
            Self::Desktop => 1 << 0,
            Self::Window => 1 << 1,
            Self::Runtime => 1 << 2,
            Self::ProcessTree => 1 << 3,
            Self::Cgroup => 1 << 4,
            Self::Manual => 1 << 5,
            Self::Launcher => 1 << 6,
        }
    }
}

/// Machine-readable explanation for one point in the detector's score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvidenceCode {
    /// An explicit user action confirmed the foreground application.
    ManualConfirmation,
    /// A matching desktop entry declares freedesktop's `Game` category.
    FreedesktopGameCategory,
    /// The foreground/focused window is fullscreen.
    FocusedFullscreen,
    /// A Proton runtime marker was observed in an app id/class or process.
    ProtonRuntime,
    /// A Wine runtime marker was observed in an app id/class or process.
    WineRuntime,
    /// A pressure-vessel runtime marker was observed.
    PressureVesselRuntime,
    /// A Sober runtime marker was observed.
    SoberRuntime,
    /// A Steam app-id marker was observed without resolving a game title.
    SteamAppIdRuntime,
    /// The selected process exposes Steam's compatibility data path key.
    SteamCompatDataPath,
    /// A Proton-specific environment key was observed.
    ProtonEnvironment,
    /// A Wine-specific environment key was observed.
    WineEnvironment,
    /// A pressure-vessel environment key was observed.
    PressureVesselEnvironment,
    /// A Sober environment key was observed.
    SoberEnvironment,
    /// A runtime process is related to the foreground process by its tree.
    ProcessTreeRuntime,
    /// A runtime marker was found in the selected process cgroup.
    GameCgroup,
    /// A known launcher process was observed.
    LauncherProcess,
    /// A known launcher window was observed.
    LauncherWindow,
}

impl EvidenceCode {
    const fn points(self) -> u16 {
        match self {
            Self::ManualConfirmation => 100,
            Self::FreedesktopGameCategory => 44,
            Self::FocusedFullscreen => 22,
            Self::ProtonRuntime => 34,
            Self::WineRuntime => 30,
            Self::PressureVesselRuntime => 28,
            Self::SoberRuntime => 34,
            Self::SteamAppIdRuntime => 24,
            Self::SteamCompatDataPath => 44,
            Self::ProtonEnvironment => 32,
            Self::WineEnvironment => 28,
            Self::PressureVesselEnvironment => 26,
            Self::SoberEnvironment => 32,
            Self::ProcessTreeRuntime => 18,
            Self::GameCgroup => 22,
            Self::LauncherProcess => 18,
            Self::LauncherWindow => 14,
        }
    }

    const fn family(self) -> EvidenceFamily {
        match self {
            Self::ManualConfirmation => EvidenceFamily::Manual,
            Self::FreedesktopGameCategory => EvidenceFamily::Desktop,
            Self::FocusedFullscreen => EvidenceFamily::Window,
            Self::ProtonRuntime
            | Self::WineRuntime
            | Self::PressureVesselRuntime
            | Self::SoberRuntime
            | Self::SteamAppIdRuntime
            | Self::SteamCompatDataPath
            | Self::ProtonEnvironment
            | Self::WineEnvironment
            | Self::PressureVesselEnvironment
            | Self::SoberEnvironment => EvidenceFamily::Runtime,
            Self::ProcessTreeRuntime => EvidenceFamily::ProcessTree,
            Self::GameCgroup => EvidenceFamily::Cgroup,
            Self::LauncherProcess | Self::LauncherWindow => EvidenceFamily::Launcher,
        }
    }

    /// A short, UI-safe explanation that contains no process names or paths.
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::ManualConfirmation => "confirmed explicitly by the user",
            Self::FreedesktopGameCategory => "matching desktop entry declares the freedesktop Game category",
            Self::FocusedFullscreen => "foreground window is fullscreen",
            Self::ProtonRuntime => "Proton runtime marker observed",
            Self::WineRuntime => "Wine runtime marker observed",
            Self::PressureVesselRuntime => "pressure-vessel runtime marker observed",
            Self::SoberRuntime => "Sober runtime marker observed",
            Self::SteamAppIdRuntime => "Steam app-id runtime marker observed",
            Self::SteamCompatDataPath => "STEAM_COMPAT_DATA_PATH key observed",
            Self::ProtonEnvironment => "Proton environment key observed",
            Self::WineEnvironment => "Wine environment key observed",
            Self::PressureVesselEnvironment => "pressure-vessel environment key observed",
            Self::SoberEnvironment => "Sober environment key observed",
            Self::ProcessTreeRuntime => "runtime process is related to the foreground process",
            Self::GameCgroup => "game-runtime marker observed in the selected cgroup",
            Self::LauncherProcess => "launcher process observed",
            Self::LauncherWindow => "launcher window observed",
        }
    }

    const fn is_runtime(self) -> bool {
        matches!(
            self,
            Self::ProtonRuntime
                | Self::WineRuntime
                | Self::PressureVesselRuntime
                | Self::SoberRuntime
                | Self::SteamAppIdRuntime
                | Self::SteamCompatDataPath
                | Self::ProtonEnvironment
                | Self::WineEnvironment
                | Self::PressureVesselEnvironment
                | Self::SoberEnvironment
        )
    }
}

/// Where a reason was observed. It intentionally avoids storing paths,
/// titles, or environment values in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvidenceOrigin {
    /// Explicit settings/user confirmation.
    Manual,
    /// A process record.
    Process { pid: u32 },
    /// A window record.
    Window { window_id: u64 },
    /// A matching desktop entry.
    DesktopEntry,
    /// A cgroup record.
    Cgroup { pid: Option<u32> },
}

/// One deduplicated, auditable reason contributing to a detection result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EvidenceReason {
    /// Stable reason code.
    pub code: EvidenceCode,
    /// Positive points assigned to this distinct signal.
    pub points: u16,
    /// Non-sensitive source location.
    pub origin: EvidenceOrigin,
}

impl EvidenceReason {
    /// Human-readable explanation suitable for diagnostics or Settings.
    pub const fn explanation(self) -> &'static str {
        self.code.explanation()
    }
}

/// The detector's output. A settings UI can show both the classification and
/// these reasons instead of presenting a mysterious on/off decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameDetection {
    /// Resulting application classification.
    pub classification: GameClassification,
    /// Deduplicated score, capped at 100.
    pub score: u16,
    /// Human-facing confidence estimate from 0 to 100.
    pub confidence_percent: u8,
    /// Process selected as foreground/target, if one was available.
    pub target_pid: Option<u32>,
    /// Number of independent evidence families involved.
    pub independent_evidence: u8,
    /// Auditable reasons, bounded by [`MAX_EVIDENCE_REASONS`].
    pub reasons: Vec<EvidenceReason>,
}

impl GameDetection {
    /// Whether automatic Game Mode may act without another confirmation.
    pub const fn eligible_for_automatic_mode(&self) -> bool {
        self.classification.is_confirmed()
    }

    /// Test whether a particular evidence code was found.
    pub fn has_evidence(&self, code: EvidenceCode) -> bool {
        self.reasons.iter().any(|reason| reason.code == code)
    }
}

/// Thresholds for automatic classification. The defaults are deliberately
/// conservative; a probable game never activates automatic mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectionThresholds {
    /// Minimum score for `ProbableGame`.
    pub probable_score: u16,
    /// Minimum score for automatic `ConfirmedGame` classification.
    pub confirmed_score: u16,
}

impl Default for DetectionThresholds {
    fn default() -> Self {
        Self {
            probable_score: 40,
            confirmed_score: 76,
        }
    }
}

/// Pure detector configured with conservative thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GameDetector {
    /// Score thresholds used by [`Self::detect`].
    pub thresholds: DetectionThresholds,
}

impl GameDetector {
    /// Detect a game from a bounded, point-in-time observation.
    pub fn detect(&self, snapshot: &GameSnapshot) -> GameDetection {
        let process_map = process_parent_map(snapshot);
        let target_pid = target_pid(snapshot);
        let active_window = active_window(snapshot, target_pid);
        let mut collector = EvidenceCollector::default();
        let mut runtime_pids = Vec::new();

        if snapshot.manual_confirmation {
            collector.add(EvidenceCode::ManualConfirmation, EvidenceOrigin::Manual);
        }

        for process in snapshot.processes.iter().take(MAX_PROCESS_SNAPSHOTS) {
            // A process record without a known foreground PID is not safely
            // attributable to the focused window. In particular, a game in
            // the background must never activate Game Mode for a browser or
            // editor that currently owns focus.
            let related = target_pid
                .map(|target| related_process(process.pid, target, &process_map))
                .unwrap_or(false);
            if !related {
                continue;
            }

            if is_launcher_process(process) {
                collector.add(
                    EvidenceCode::LauncherProcess,
                    EvidenceOrigin::Process { pid: process.pid },
                );
            }

            if let Some(marker) = process_runtime_marker(process) {
                collector.add(marker.code(), EvidenceOrigin::Process { pid: process.pid });
                runtime_pids.push(process.pid);
            }

            for key in process.environment_keys.iter().take(MAX_EVIDENCE_REASONS) {
                if let Some(code) = environment_evidence_code(key) {
                    collector.add(code, EvidenceOrigin::Process { pid: process.pid });
                }
            }
        }

        if let Some(window) = active_window {
            if window.fullscreen {
                collector.add(
                    EvidenceCode::FocusedFullscreen,
                    EvidenceOrigin::Window {
                        window_id: window.window_id,
                    },
                );
            }

            for text in [window.app_id.as_deref(), window.wm_class.as_deref()]
                .into_iter()
                .flatten()
            {
                if let Some(marker) = runtime_marker_in_text(text) {
                    collector.add(
                        marker.code(),
                        EvidenceOrigin::Window {
                            window_id: window.window_id,
                        },
                    );
                }
                if is_launcher_text(text) {
                    collector.add(
                        EvidenceCode::LauncherWindow,
                        EvidenceOrigin::Window {
                            window_id: window.window_id,
                        },
                    );
                }
            }
        }

        let launcher_target = target_is_launcher(snapshot, target_pid, active_window);
        let has_runtime_before_desktop = collector.has_runtime_evidence();
        for entry in snapshot.desktop_entries.iter().take(MAX_DESKTOP_ENTRIES) {
            if !desktop_entry_matches(entry, snapshot, target_pid, active_window) {
                continue;
            }
            if has_game_category(entry)
                && (!launcher_target
                    || has_runtime_before_desktop
                    || active_window_is_fullscreen(active_window))
            {
                collector.add(
                    EvidenceCode::FreedesktopGameCategory,
                    EvidenceOrigin::DesktopEntry,
                );
            }
        }

        for cgroup in snapshot.cgroups.iter().take(MAX_CGROUP_SNAPSHOTS) {
            let related = match (cgroup.pid, target_pid) {
                (Some(pid), Some(target)) => related_process(pid, target, &process_map),
                // An unscoped process/cgroup observation is not evidence
                // until the adapter can associate it with the foreground
                // application. This avoids false positives with a running
                // game on another workspace or in another seat.
                (Some(_), None) => false,
                (None, None) => false,
                // An unscoped cgroup must not influence an unrelated focused
                // app; this is important when the user has background games.
                (None, Some(_)) => false,
            };
            if related && runtime_marker_in_text(&cgroup.path).is_some() {
                collector.add(
                    EvidenceCode::GameCgroup,
                    EvidenceOrigin::Cgroup { pid: cgroup.pid },
                );
            }
        }

        if let Some(target) = target_pid {
            if runtime_pids
                .iter()
                .any(|pid| *pid != target && related_process(*pid, target, &process_map))
            {
                collector.add(
                    EvidenceCode::ProcessTreeRuntime,
                    EvidenceOrigin::Process { pid: target },
                );
            }
        }

        let classification = classify(
            &collector,
            self.thresholds,
            snapshot.processes.iter().take(MAX_PROCESS_SNAPSHOTS).count(),
            snapshot.windows.iter().take(MAX_WINDOW_SNAPSHOTS).count(),
        );
        let confidence_percent = confidence(classification, collector.score, snapshot.manual_confirmation);

        GameDetection {
            classification,
            score: collector.score,
            confidence_percent,
            target_pid,
            independent_evidence: collector.independent_families(),
            reasons: collector.reasons,
        }
    }
}

/// Deduplicates codes before scoring so ten copies of one marker cannot inflate
/// confidence or cause an OOM-like diagnostic allocation.
#[derive(Debug, Default)]
struct EvidenceCollector {
    score: u16,
    family_bits: u8,
    reasons: Vec<EvidenceReason>,
}

impl EvidenceCollector {
    fn add(&mut self, code: EvidenceCode, origin: EvidenceOrigin) {
        if self.reasons.iter().any(|reason| reason.code == code) {
            return;
        }
        if self.reasons.len() >= MAX_EVIDENCE_REASONS {
            return;
        }
        self.score = self.score.saturating_add(code.points()).min(100);
        self.family_bits |= code.family().bit();
        self.reasons.push(EvidenceReason {
            code,
            points: code.points(),
            origin,
        });
    }

    fn has(&self, code: EvidenceCode) -> bool {
        self.reasons.iter().any(|reason| reason.code == code)
    }

    fn has_runtime_evidence(&self) -> bool {
        self.reasons.iter().any(|reason| reason.code.is_runtime())
    }

    fn has_game_context(&self) -> bool {
        self.reasons.iter().any(|reason| {
            matches!(
                reason.code,
                EvidenceCode::FreedesktopGameCategory
                    | EvidenceCode::ProtonRuntime
                    | EvidenceCode::WineRuntime
                    | EvidenceCode::PressureVesselRuntime
                    | EvidenceCode::SoberRuntime
                    | EvidenceCode::SteamAppIdRuntime
                    | EvidenceCode::SteamCompatDataPath
                    | EvidenceCode::ProtonEnvironment
                    | EvidenceCode::WineEnvironment
                    | EvidenceCode::PressureVesselEnvironment
                    | EvidenceCode::SoberEnvironment
                    | EvidenceCode::ProcessTreeRuntime
                    | EvidenceCode::GameCgroup
            )
        })
    }

    fn has_strong_runtime(&self) -> bool {
        self.reasons.iter().any(|reason| {
            matches!(
                reason.code,
                EvidenceCode::ProtonRuntime
                    | EvidenceCode::WineRuntime
                    | EvidenceCode::PressureVesselRuntime
                    | EvidenceCode::SoberRuntime
                    | EvidenceCode::SteamCompatDataPath
                    | EvidenceCode::ProtonEnvironment
                    | EvidenceCode::WineEnvironment
                    | EvidenceCode::PressureVesselEnvironment
                    | EvidenceCode::SoberEnvironment
            )
        })
    }

    fn independent_families(&self) -> u8 {
        self.family_bits.count_ones() as u8
    }
}

fn classify(
    collector: &EvidenceCollector,
    thresholds: DetectionThresholds,
    process_count: usize,
    window_count: usize,
) -> GameClassification {
    if collector.has(EvidenceCode::ManualConfirmation) {
        return GameClassification::ConfirmedGame;
    }

    let game_context = collector.has_game_context();
    let strong_shape = collector.has(EvidenceCode::FreedesktopGameCategory)
        || collector.has(EvidenceCode::FocusedFullscreen)
        || collector.has(EvidenceCode::ProcessTreeRuntime)
        || collector.has(EvidenceCode::GameCgroup);
    let confirmed = collector.score >= thresholds.confirmed_score
        && collector.independent_families() >= 2
        && collector.has_strong_runtime()
        && strong_shape;

    if confirmed {
        GameClassification::ConfirmedGame
    } else if game_context && collector.score >= thresholds.probable_score {
        GameClassification::ProbableGame
    } else if collector.has(EvidenceCode::LauncherProcess) || collector.has(EvidenceCode::LauncherWindow) {
        GameClassification::Launcher
    } else if process_count > 0 || window_count > 0 {
        GameClassification::CommonApp
    } else {
        GameClassification::Unknown
    }
}

fn confidence(classification: GameClassification, score: u16, manual: bool) -> u8 {
    match classification {
        GameClassification::Unknown => 0,
        GameClassification::CommonApp => 12,
        GameClassification::Launcher => 35u16.saturating_add(score.min(30)) as u8,
        GameClassification::ProbableGame => 40u16.saturating_add(score / 2).min(89) as u8,
        GameClassification::ConfirmedGame if manual => 100,
        GameClassification::ConfirmedGame => 80u16.saturating_add(score / 5).min(99) as u8,
    }
}

fn target_pid(snapshot: &GameSnapshot) -> Option<u32> {
    snapshot.foreground_pid.or_else(|| {
        snapshot
            .windows
            .iter()
            .take(MAX_WINDOW_SNAPSHOTS)
            .find(|window| window.focused)
            .and_then(|window| window.pid)
    })
}

fn active_window(snapshot: &GameSnapshot, target_pid: Option<u32>) -> Option<&WindowSnapshot> {
    snapshot.windows.iter().take(MAX_WINDOW_SNAPSHOTS).find(|window| {
        window.focused
            || target_pid
                .zip(window.pid)
                .is_some_and(|(target, pid)| target == pid)
    })
}

fn active_window_is_fullscreen(window: Option<&WindowSnapshot>) -> bool {
    window.is_some_and(|window| window.fullscreen)
}

fn process_parent_map(snapshot: &GameSnapshot) -> HashMap<u32, Option<u32>> {
    snapshot
        .processes
        .iter()
        .take(MAX_PROCESS_SNAPSHOTS)
        .map(|process| (process.pid, process.parent_pid))
        .collect()
}

fn related_process(a: u32, b: u32, parent_map: &HashMap<u32, Option<u32>>) -> bool {
    a == b || is_ancestor(a, b, parent_map) || is_ancestor(b, a, parent_map)
}

fn is_ancestor(candidate_ancestor: u32, child: u32, parent_map: &HashMap<u32, Option<u32>>) -> bool {
    let mut current = child;
    for _ in 0..64 {
        let Some(parent) = parent_map.get(&current).copied().flatten() else {
            return false;
        };
        if parent == candidate_ancestor {
            return true;
        }
        if parent == current {
            return false;
        }
        current = parent;
    }
    false
}

fn process_runtime_marker(process: &ProcessSnapshot) -> Option<RuntimeMarker> {
    runtime_marker_in_texts(
        std::iter::once(process.executable.as_str()).chain(process.argv.iter().map(String::as_str)),
    )
}

fn runtime_marker_in_texts<'a>(texts: impl Iterator<Item = &'a str>) -> Option<RuntimeMarker> {
    texts.into_iter().find_map(runtime_marker_in_text)
}

fn runtime_marker_in_text(text: &str) -> Option<RuntimeMarker> {
    let lower = text.to_ascii_lowercase();
    for component in lower.split(|character: char| {
        character.is_ascii_whitespace() || matches!(character, '/' | '\\' | ':' | '=' | '.')
    }) {
        if component.is_empty() {
            continue;
        }
        if component.starts_with("steam_app_") {
            return Some(RuntimeMarker::SteamAppId);
        }
        if component == "sober" || component.starts_with("sober-") {
            return Some(RuntimeMarker::Sober);
        }
        if component == "proton" || component.starts_with("proton-") || component.starts_with("proton_") {
            return Some(RuntimeMarker::Proton);
        }
        if component == "pressure-vessel"
            || component.starts_with("pressure-vessel-")
            || component == "pressure_vessel"
            || component.starts_with("pressure_vessel_")
        {
            return Some(RuntimeMarker::PressureVessel);
        }
        if component == "wine"
            || component == "wine32"
            || component == "wine64"
            || component == "wineserver"
            || component == "wineboot"
            || component == "winecfg"
            || component == "wine-preloader"
        {
            return Some(RuntimeMarker::Wine);
        }
    }
    None
}

fn is_launcher_process(process: &ProcessSnapshot) -> bool {
    is_launcher_text(&process.executable) || process.argv.iter().any(|argument| is_launcher_text(argument))
}

fn is_launcher_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '/' | '\\' | ':' | '=' | '.')
        })
        .any(|token| {
            matches!(
                token,
                "steam" | "steamwebhelper" | "lutris" | "heroic" | "bottles"
            )
        })
}

fn environment_evidence_code(key: &str) -> Option<EvidenceCode> {
    let key = key.trim().to_ascii_uppercase();
    match key.as_str() {
        "STEAM_COMPAT_DATA_PATH" => Some(EvidenceCode::SteamCompatDataPath),
        key if key.starts_with("PROTON_") => Some(EvidenceCode::ProtonEnvironment),
        "WINEPREFIX" | "WINEARCH" | "WINELOADER" => Some(EvidenceCode::WineEnvironment),
        key if key.starts_with("WINE_") => Some(EvidenceCode::WineEnvironment),
        key if key.starts_with("PRESSURE_VESSEL_") => Some(EvidenceCode::PressureVesselEnvironment),
        "SOBER" => Some(EvidenceCode::SoberEnvironment),
        key if key.starts_with("SOBER_") => Some(EvidenceCode::SoberEnvironment),
        // A Steam app id is useful context, but alone it is intentionally
        // weaker than STEAM_COMPAT_DATA_PATH because Steam itself also has
        // Steam-related environment variables.
        "STEAM_APPID" | "STEAM_GAME_ID" => Some(EvidenceCode::SteamAppIdRuntime),
        _ => None,
    }
}

fn has_game_category(entry: &DesktopEntrySnapshot) -> bool {
    entry
        .categories
        .iter()
        .any(|category| category.trim().trim_end_matches(';').eq_ignore_ascii_case("game"))
}

fn desktop_entry_matches(
    entry: &DesktopEntrySnapshot,
    snapshot: &GameSnapshot,
    target_pid: Option<u32>,
    active_window: Option<&WindowSnapshot>,
) -> bool {
    if entry.pid.is_some_and(|pid| Some(pid) == target_pid) {
        return true;
    }

    if let Some(target) = target_pid {
        if snapshot
            .processes
            .iter()
            .take(MAX_PROCESS_SNAPSHOTS)
            .any(|process| {
                process.pid == target
                    && process
                        .desktop_id
                        .as_deref()
                        .is_some_and(|desktop_id| desktop_id.eq_ignore_ascii_case(&entry.desktop_id))
            })
        {
            return true;
        }
    }

    let Some(window) = active_window else {
        return false;
    };
    if entry.pid.is_some_and(|pid| Some(pid) == window.pid) {
        return true;
    }
    entry.app_id.as_deref().is_some_and(|app_id| {
        window
            .app_id
            .as_deref()
            .is_some_and(|window_app_id| window_app_id.eq_ignore_ascii_case(app_id))
    }) || entry.wm_class.as_deref().is_some_and(|wm_class| {
        window
            .wm_class
            .as_deref()
            .is_some_and(|window_class| window_class.eq_ignore_ascii_case(wm_class))
    })
}

fn target_is_launcher(
    snapshot: &GameSnapshot,
    target_pid: Option<u32>,
    active_window: Option<&WindowSnapshot>,
) -> bool {
    if target_pid.is_some_and(|target| {
        snapshot
            .processes
            .iter()
            .take(MAX_PROCESS_SNAPSHOTS)
            .any(|process| process.pid == target && is_launcher_process(process))
    }) {
        return true;
    }
    active_window.is_some_and(|window| {
        window.app_id.as_deref().is_some_and(is_launcher_text)
            || window.wm_class.as_deref().is_some_and(is_launcher_text)
    })
}

impl RuntimeMarker {
    const fn code(self) -> EvidenceCode {
        match self {
            Self::Proton => EvidenceCode::ProtonRuntime,
            Self::Wine => EvidenceCode::WineRuntime,
            Self::PressureVessel => EvidenceCode::PressureVesselRuntime,
            Self::Sober => EvidenceCode::SoberRuntime,
            Self::SteamAppId => EvidenceCode::SteamAppIdRuntime,
        }
    }
}

/// Automatic, user-confirmed, or disabled Game Mode operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GameModePreference {
    /// Act only after the detector reaches `ConfirmedGame`.
    #[default]
    Automatic,
    /// Act only when the Settings toggle explicitly requests it.
    Manual,
    /// Never enter Game Mode automatically or manually through reconciliation.
    Disabled,
}

/// Broad hardware tier used to choose how much optional shell work to pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PerformanceTier {
    /// Keep more of the visual treatment while removing background work.
    HighEnd,
    /// A balanced compromise for ordinary hardware.
    #[default]
    Balanced,
    /// Protect memory and frame pacing on low-end hardware.
    LowEnd,
}

impl PerformanceTier {
    /// Choose a tier from measurable host capacity instead of a machine-name
    /// guess. The thresholds only select how much optional shell work is
    /// allowed; they do not claim a benchmark or game-performance result.
    pub const fn from_hardware(memory_bytes: u64, logical_cpus: usize) -> Self {
        const GIB: u64 = 1024 * 1024 * 1024;
        if memory_bytes <= 4 * GIB || logical_cpus <= 2 {
            Self::LowEnd
        } else if memory_bytes >= 16 * GIB && logical_cpus >= 8 {
            Self::HighEnd
        } else {
            Self::Balanced
        }
    }
}

/// The optional shell effects Game Mode is allowed to reduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameModeEffects {
    /// Reduce or disable expensive blur sampling in the shell.
    pub reduce_blur: bool,
    /// Reduce shell transparency so fewer composited layers remain live.
    pub reduce_transparency: bool,
    /// Pause nonessential background reads.
    pub pause_background_reads: bool,
    /// Pause widget refresh/timers.
    pub pause_widget_refresh: bool,
    /// Pause App Gallery/Flatpak catalogue refreshes.
    pub pause_gallery_refresh: bool,
    /// Optional redraw cap for shell surfaces; `None` keeps the current cap.
    pub redraw_cap_fps: Option<u16>,
    /// Stop optional shell animations while the game owns the foreground.
    pub limit_animations: bool,
    /// Hold noncritical notifications until the game ends.
    pub defer_noncritical_notifications: bool,
}

impl GameModeEffects {
    /// Select a bounded policy without probing hardware or changing state.
    pub const fn for_tier(tier: PerformanceTier) -> Self {
        match tier {
            PerformanceTier::HighEnd => Self {
                reduce_blur: true,
                reduce_transparency: false,
                pause_background_reads: true,
                pause_widget_refresh: true,
                pause_gallery_refresh: true,
                redraw_cap_fps: Some(60),
                limit_animations: true,
                defer_noncritical_notifications: true,
            },
            PerformanceTier::Balanced => Self {
                reduce_blur: true,
                reduce_transparency: true,
                pause_background_reads: true,
                pause_widget_refresh: true,
                pause_gallery_refresh: true,
                redraw_cap_fps: Some(45),
                limit_animations: true,
                defer_noncritical_notifications: true,
            },
            PerformanceTier::LowEnd => Self {
                reduce_blur: true,
                reduce_transparency: true,
                pause_background_reads: true,
                pause_widget_refresh: true,
                pause_gallery_refresh: true,
                redraw_cap_fps: Some(30),
                limit_animations: true,
                defer_noncritical_notifications: true,
            },
        }
    }
}

/// Resources that are never disabled by Game Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtectedResources {
    input: bool,
    compositor: bool,
    audio_and_foreground: bool,
    security: bool,
}

impl ProtectedResources {
    const fn always_on() -> Self {
        Self {
            input: true,
            compositor: true,
            audio_and_foreground: true,
            security: true,
        }
    }

    /// Input handling is always retained.
    pub const fn input(self) -> bool {
        self.input
    }

    /// The compositor/event loop is always retained.
    pub const fn compositor(self) -> bool {
        self.compositor
    }

    /// Audio and foreground ownership are always retained.
    pub const fn audio_and_foreground(self) -> bool {
        self.audio_and_foreground
    }

    /// Lock/security behavior is always retained.
    pub const fn security(self) -> bool {
        self.security
    }
}

/// Complete resource policy returned to Settings/integration code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameModePolicy {
    /// Optional effects that may be applied to the shell.
    pub effects: GameModeEffects,
    protected: ProtectedResources,
}

impl GameModePolicy {
    /// Build the safe policy for a hardware tier.
    pub const fn for_tier(tier: PerformanceTier) -> Self {
        Self {
            effects: GameModeEffects::for_tier(tier),
            protected: ProtectedResources::always_on(),
        }
    }

    /// Return the resources that the policy guarantees remain active.
    pub const fn protected_resources(self) -> ProtectedResources {
        self.protected
    }

    /// True when all non-negotiable resources are retained.
    pub const fn is_safe(self) -> bool {
        self.protected.input
            && self.protected.compositor
            && self.protected.audio_and_foreground
            && self.protected.security
    }

    /// Apply only the permitted shell reductions to a typed shell snapshot.
    pub fn apply_to(self, before: ShellState) -> ShellState {
        let mut after = before;
        after.battery_saver = true;
        if self.effects.reduce_blur {
            after.blur_enabled = false;
        }
        if self.effects.reduce_transparency {
            after.transparency_enabled = false;
        }
        if self.effects.pause_background_reads {
            after.background_reads_enabled = false;
        }
        if self.effects.pause_widget_refresh {
            after.widget_refresh_enabled = false;
        }
        if self.effects.pause_gallery_refresh {
            after.gallery_refresh_enabled = false;
        }
        if let Some(cap) = self.effects.redraw_cap_fps {
            after.redraw_fps = if before.redraw_fps == 0 {
                cap
            } else {
                before.redraw_fps.min(cap)
            };
        }
        if self.effects.limit_animations {
            after.animations_enabled = false;
        }
        if self.effects.defer_noncritical_notifications {
            after.noncritical_notifications_enabled = false;
        }
        after
    }
}

/// The shell preferences/state captured before Game Mode enters.
///
/// This type intentionally contains only bounded booleans and a small numeric
/// value. It cannot hold credentials, paths, process output, or arbitrary
/// serialized settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellState {
    /// Whether the shell's battery-saving profile is active.
    pub battery_saver: bool,
    /// Whether shell blur is active.
    pub blur_enabled: bool,
    /// Whether shell transparency is active.
    pub transparency_enabled: bool,
    /// Whether background shell reads are active.
    pub background_reads_enabled: bool,
    /// Whether widgets refresh.
    pub widget_refresh_enabled: bool,
    /// Whether the App Gallery refreshes.
    pub gallery_refresh_enabled: bool,
    /// Current shell redraw cap; zero means uncapped.
    pub redraw_fps: u16,
    /// Whether optional shell animations are active.
    pub animations_enabled: bool,
    /// Whether noncritical notifications can be delivered immediately.
    pub noncritical_notifications_enabled: bool,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            battery_saver: false,
            blur_enabled: true,
            transparency_enabled: true,
            background_reads_enabled: true,
            widget_refresh_enabled: true,
            gallery_refresh_enabled: true,
            redraw_fps: 0,
            animations_enabled: true,
            noncritical_notifications_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CachedShellState {
    version: u16,
    state: ShellState,
}

/// Bounded, versioned temporary cache for exactly one game session's shell
/// state. It never accepts arbitrary bytes or secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporaryStateCache {
    capacity_bytes: usize,
    entry: Option<CachedShellState>,
}

impl Default for TemporaryStateCache {
    fn default() -> Self {
        Self::with_capacity(MAX_CACHE_BYTES)
    }
}

impl TemporaryStateCache {
    /// Create a cache, clamping the requested quota to the hard upper bound.
    pub fn with_capacity(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes: capacity_bytes.min(MAX_CACHE_BYTES),
            entry: None,
        }
    }

    /// The cache schema version that will be written/read.
    pub const fn version(&self) -> u16 {
        CACHE_FORMAT_VERSION
    }

    /// Configured logical quota in bytes.
    pub const fn capacity_bytes(&self) -> usize {
        self.capacity_bytes
    }

    /// Whether a saved state currently occupies the one cache slot.
    pub const fn is_full(&self) -> bool {
        self.entry.is_some()
    }

    /// Save a typed shell state without overwriting an existing session.
    pub fn store(&mut self, state: ShellState) -> Result<(), CacheError> {
        if self.entry.is_some() {
            return Err(CacheError::Full);
        }
        if self.capacity_bytes < CACHED_SHELL_STATE_BYTES {
            return Err(CacheError::CapacityTooSmall);
        }
        self.entry = Some(CachedShellState {
            version: CACHE_FORMAT_VERSION,
            state,
        });
        Ok(())
    }

    /// Take the saved state once. A second call is a safe no-op.
    pub fn take(&mut self) -> Option<ShellState> {
        let entry = self.entry.take()?;
        (entry.version == CACHE_FORMAT_VERSION).then_some(entry.state)
    }

    /// Drop a stale cache entry explicitly.
    pub fn clear(&mut self) {
        self.entry = None;
    }
}

/// Errors that stop a Game Mode transition before shell state is changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheError {
    /// A game session is already occupying the only cache slot.
    Full,
    /// The configured quota is smaller than one typed state entry.
    CapacityTooSmall,
}

/// Errors from a safe enter/exit transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameModeError {
    /// The temporary cache could not preserve the previous shell state.
    Cache(CacheError),
    /// The user explicitly disabled Game Mode.
    Disabled,
    /// An internal invariant was broken; the active state remains untouched.
    MissingCachedState,
}

/// Observable result of reconciling Game Mode with the latest detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameModeTransition {
    /// Game Mode entered and this is the shell state the adapter should apply.
    Entered {
        state: ShellState,
        policy: GameModePolicy,
    },
    /// Game Mode exited and this exact pre-game state should be restored.
    Exited { restored: ShellState },
    /// No state change was needed.
    NoChange { active: bool },
}

/// Idempotent controller intended for `settings.rs` or a future Linux adapter.
///
/// It returns state/policy values rather than performing privileged actions.
/// The caller remains responsible for applying redraw, notification, and
/// refresh settings through its existing shell APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameModeController {
    preference: GameModePreference,
    tier: PerformanceTier,
    active: bool,
    cache: TemporaryStateCache,
}

impl Default for GameModeController {
    fn default() -> Self {
        Self::new(GameModePreference::Automatic, PerformanceTier::Balanced)
    }
}

impl GameModeController {
    /// Create a controller with the default bounded cache.
    pub fn new(preference: GameModePreference, tier: PerformanceTier) -> Self {
        Self {
            preference,
            tier,
            active: false,
            cache: TemporaryStateCache::default(),
        }
    }

    /// Create a controller with a testable/custom bounded cache quota.
    pub fn with_cache_capacity(
        preference: GameModePreference,
        tier: PerformanceTier,
        capacity_bytes: usize,
    ) -> Self {
        Self {
            preference,
            tier,
            active: false,
            cache: TemporaryStateCache::with_capacity(capacity_bytes),
        }
    }

    /// Current user preference.
    pub const fn preference(&self) -> GameModePreference {
        self.preference
    }

    /// Change preference without entering or exiting by itself.
    pub fn set_preference(&mut self, preference: GameModePreference) {
        self.preference = preference;
    }

    /// Current hardware/performance tier.
    pub const fn tier(&self) -> PerformanceTier {
        self.tier
    }

    /// Change the policy tier for the next entry.
    pub fn set_tier(&mut self, tier: PerformanceTier) {
        self.tier = tier;
    }

    /// Whether a game policy is currently active.
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Whether the bounded temporary snapshot currently holds a shell state.
    pub const fn cache_full(&self) -> bool {
        self.cache.is_full()
    }

    /// Enter directly after an explicit manual Settings action.
    pub fn enter(&mut self, current_shell: ShellState) -> Result<GameModeTransition, GameModeError> {
        if self.preference == GameModePreference::Disabled {
            return Err(GameModeError::Disabled);
        }
        if self.active {
            return Ok(GameModeTransition::NoChange { active: true });
        }
        self.cache.store(current_shell).map_err(GameModeError::Cache)?;
        self.active = true;
        let policy = GameModePolicy::for_tier(self.tier);
        Ok(GameModeTransition::Entered {
            state: policy.apply_to(current_shell),
            policy,
        })
    }

    /// Exit and restore the exact pre-game state. A repeated exit is harmless.
    pub fn exit(&mut self) -> Result<GameModeTransition, GameModeError> {
        if !self.active {
            return Ok(GameModeTransition::NoChange { active: false });
        }
        let Some(restored) = self.cache.take() else {
            return Err(GameModeError::MissingCachedState);
        };
        self.active = false;
        Ok(GameModeTransition::Exited { restored })
    }

    /// Reconcile the controller with detection and the current shell state.
    ///
    /// Automatic mode requires `ConfirmedGame`; a probable/unknown app never
    /// changes shell policy. Manual mode requires `manual_requested`, and Off
    /// always restores an already-active session.
    pub fn reconcile(
        &mut self,
        detection: &GameDetection,
        current_shell: ShellState,
        manual_requested: bool,
    ) -> Result<GameModeTransition, GameModeError> {
        let should_activate = match self.preference {
            GameModePreference::Automatic => detection.eligible_for_automatic_mode(),
            GameModePreference::Manual => manual_requested,
            GameModePreference::Disabled => false,
        };

        match (should_activate, self.active) {
            (true, false) => self.enter(current_shell),
            (false, true) => self.exit(),
            (_, active) => Ok(GameModeTransition::NoChange { active }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, parent_pid: Option<u32>, executable: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            parent_pid,
            executable: executable.into(),
            ..ProcessSnapshot::default()
        }
    }

    fn focused_window(pid: u32, app_id: &str, fullscreen: bool) -> WindowSnapshot {
        WindowSnapshot {
            window_id: pid as u64,
            pid: Some(pid),
            app_id: Some(app_id.into()),
            fullscreen,
            focused: true,
            ..WindowSnapshot::default()
        }
    }

    fn confirmed_snapshot() -> GameSnapshot {
        let mut proton = process(21, Some(20), "/usr/bin/proton");
        proton.environment_keys.push("STEAM_COMPAT_DATA_PATH".into());
        GameSnapshot {
            processes: vec![
                process(10, None, "/usr/bin/steam"),
                process(20, Some(10), "/opt/game-binary"),
                proton,
            ],
            windows: vec![focused_window(20, "steam_app_12345", true)],
            desktop_entries: vec![DesktopEntrySnapshot {
                desktop_id: "example-game.desktop".into(),
                categories: vec!["Game;".into()],
                pid: Some(20),
                ..DesktopEntrySnapshot::default()
            }],
            foreground_pid: Some(20),
            ..GameSnapshot::default()
        }
    }

    #[test]
    fn empty_snapshot_is_unknown_and_cannot_activate() {
        let detection = GameDetector::default().detect(&GameSnapshot::default());
        assert_eq!(detection.classification, GameClassification::Unknown);
        assert_eq!(detection.score, 0);
        assert!(!detection.eligible_for_automatic_mode());
    }

    #[test]
    fn hardware_tier_uses_capacity_not_machine_names() {
        assert_eq!(
            PerformanceTier::from_hardware(4 * 1024 * 1024 * 1024, 4),
            PerformanceTier::LowEnd
        );
        assert_eq!(
            PerformanceTier::from_hardware(32 * 1024 * 1024 * 1024, 12),
            PerformanceTier::HighEnd
        );
        assert_eq!(
            PerformanceTier::from_hardware(8 * 1024 * 1024 * 1024, 4),
            PerformanceTier::Balanced
        );
    }

    #[test]
    fn similar_process_names_are_not_game_evidence() {
        let snapshot = GameSnapshot {
            processes: vec![
                process(7, None, "/opt/my-game-helper"),
                process(8, None, "/opt/my-proton-game"),
                process(9, None, "/opt/my-steam-game"),
            ],
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&snapshot);
        assert_eq!(detection.classification, GameClassification::CommonApp);
        assert_eq!(detection.score, 0);
        assert!(detection.reasons.is_empty());
    }

    #[test]
    fn fullscreen_alone_is_not_a_game() {
        let snapshot = GameSnapshot {
            processes: vec![process(7, None, "/usr/bin/browser")],
            windows: vec![focused_window(7, "org.example.Browser", true)],
            foreground_pid: Some(7),
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&snapshot);
        assert_eq!(detection.classification, GameClassification::CommonApp);
        assert_eq!(detection.score, 22);
        assert!(!detection.eligible_for_automatic_mode());
    }

    #[test]
    fn steam_without_runtime_is_a_launcher() {
        let snapshot = GameSnapshot {
            processes: vec![process(10, None, "/usr/bin/steam")],
            windows: vec![focused_window(10, "com.valvesoftware.Steam", false)],
            foreground_pid: Some(10),
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&snapshot);
        assert_eq!(detection.classification, GameClassification::Launcher);
        assert!(detection.has_evidence(EvidenceCode::LauncherProcess));
        assert!(!detection.has_evidence(EvidenceCode::FreedesktopGameCategory));
    }

    #[test]
    fn proton_requires_context_before_automatic_confirmation() {
        let process_only = GameSnapshot {
            processes: vec![process(20, None, "/usr/bin/proton")],
            foreground_pid: Some(20),
            ..GameSnapshot::default()
        };
        let probable = GameDetector::default().detect(&process_only);
        assert_ne!(probable.classification, GameClassification::ConfirmedGame);
        assert!(!probable.eligible_for_automatic_mode());

        let mut with_context = process_only;
        with_context.windows = vec![focused_window(20, "steam_app_54321", true)];
        let confirmed = GameDetector::default().detect(&with_context);
        assert_eq!(confirmed.classification, GameClassification::ConfirmedGame);
        assert!(confirmed.has_evidence(EvidenceCode::ProtonRuntime));
        assert!(confirmed.has_evidence(EvidenceCode::FocusedFullscreen));
    }

    #[test]
    fn sober_is_detected_by_runtime_evidence_not_a_game_title() {
        let snapshot = GameSnapshot {
            processes: vec![process(30, None, "/usr/bin/sober")],
            windows: vec![focused_window(30, "org.vinegarhq.Sober", true)],
            foreground_pid: Some(30),
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&snapshot);
        assert_eq!(detection.classification, GameClassification::ProbableGame);
        assert!(detection.has_evidence(EvidenceCode::SoberRuntime));
        assert!(!detection.eligible_for_automatic_mode());
    }

    #[test]
    fn proton_tree_and_game_category_confirm_automatically() {
        let detection = GameDetector::default().detect(&confirmed_snapshot());
        assert_eq!(detection.classification, GameClassification::ConfirmedGame);
        assert_eq!(detection.target_pid, Some(20));
        assert!(detection.independent_evidence >= 2);
        assert!(detection.has_evidence(EvidenceCode::SteamCompatDataPath));
        assert!(detection.has_evidence(EvidenceCode::ProcessTreeRuntime));
        assert!(
            detection
                .reasons
                .iter()
                .all(|reason| !matches!(reason.origin, EvidenceOrigin::Manual))
        );
    }

    #[test]
    fn desktop_game_category_only_applies_to_matching_foreground_entry() {
        let installed_only = GameSnapshot {
            desktop_entries: vec![DesktopEntrySnapshot {
                desktop_id: "game.desktop".into(),
                categories: vec!["Game".into()],
                ..DesktopEntrySnapshot::default()
            }],
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&installed_only);
        assert_eq!(detection.classification, GameClassification::Unknown);

        let matching = GameSnapshot {
            processes: vec![process(8, None, "/opt/game-runtime")],
            windows: vec![focused_window(8, "org.example.Game", false)],
            foreground_pid: Some(8),
            desktop_entries: vec![DesktopEntrySnapshot {
                desktop_id: "game.desktop".into(),
                categories: vec!["Game".into()],
                app_id: Some("org.example.Game".into()),
                ..DesktopEntrySnapshot::default()
            }],
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&matching);
        assert_eq!(detection.classification, GameClassification::ProbableGame);
        assert!(detection.has_evidence(EvidenceCode::FreedesktopGameCategory));
    }

    #[test]
    fn duplicate_runtime_markers_are_capped_and_reasons_are_bounded() {
        let processes = (1..=MAX_PROCESS_SNAPSHOTS as u32)
            .map(|pid| process(pid, None, "/usr/bin/proton"))
            .collect();
        let snapshot = GameSnapshot {
            processes,
            foreground_pid: Some(1),
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&snapshot);
        assert_eq!(detection.score, EvidenceCode::ProtonRuntime.points());
        assert!(detection.reasons.len() <= MAX_EVIDENCE_REASONS);
    }

    #[test]
    fn background_runtime_without_foreground_pid_cannot_activate_game_mode() {
        let snapshot = GameSnapshot {
            processes: vec![process(77, None, "/usr/bin/proton")],
            windows: vec![focused_window(12, "org.example.Browser", false)],
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&snapshot);
        assert_eq!(detection.classification, GameClassification::CommonApp);
        assert!(!detection.eligible_for_automatic_mode());
        assert!(!detection.has_evidence(EvidenceCode::ProtonRuntime));
    }

    #[test]
    fn manual_confirmation_can_confirm_without_name_matching() {
        let snapshot = GameSnapshot {
            processes: vec![process(8, None, "/opt/anything")],
            manual_confirmation: true,
            ..GameSnapshot::default()
        };
        let detection = GameDetector::default().detect(&snapshot);
        assert_eq!(detection.classification, GameClassification::ConfirmedGame);
        assert_eq!(detection.confidence_percent, 100);
        assert!(detection.eligible_for_automatic_mode());
    }

    #[test]
    fn cache_is_versioned_bounded_and_rejects_a_second_entry() {
        let mut cache = TemporaryStateCache::default();
        let state = ShellState::default();
        assert_eq!(cache.version(), CACHE_FORMAT_VERSION);
        assert!(cache.store(state).is_ok());
        assert!(cache.is_full());
        assert_eq!(cache.store(state), Err(CacheError::Full));
        assert_eq!(cache.take(), Some(state));
        assert_eq!(cache.take(), None);

        let mut too_small = TemporaryStateCache::with_capacity(CACHED_SHELL_STATE_BYTES - 1);
        assert_eq!(too_small.store(state), Err(CacheError::CapacityTooSmall));
        assert!(TemporaryStateCache::with_capacity(MAX_CACHE_BYTES * 2).capacity_bytes() <= MAX_CACHE_BYTES);
    }

    #[test]
    fn policy_keeps_non_negotiable_resources_and_applies_only_optional_effects() {
        let policy = GameModePolicy::for_tier(PerformanceTier::LowEnd);
        let protected = policy.protected_resources();
        assert!(policy.is_safe());
        assert!(protected.input());
        assert!(protected.compositor());
        assert!(protected.audio_and_foreground());
        assert!(protected.security());

        let state = ShellState::default();
        let reduced = policy.apply_to(state);
        assert!(!reduced.blur_enabled);
        assert!(!reduced.transparency_enabled);
        assert!(!reduced.background_reads_enabled);
        assert!(!reduced.widget_refresh_enabled);
        assert!(!reduced.gallery_refresh_enabled);
        assert_eq!(reduced.redraw_fps, 30);
        assert!(!reduced.animations_enabled);
        assert!(!reduced.noncritical_notifications_enabled);
    }

    #[test]
    fn controller_enters_exits_and_restores_idempotently() {
        let detection = GameDetector::default().detect(&confirmed_snapshot());
        let original = ShellState {
            battery_saver: true,
            blur_enabled: false,
            transparency_enabled: true,
            background_reads_enabled: false,
            widget_refresh_enabled: true,
            gallery_refresh_enabled: false,
            redraw_fps: 90,
            animations_enabled: false,
            noncritical_notifications_enabled: true,
        };
        let mut controller = GameModeController::default();

        let entered = controller
            .reconcile(&detection, original, false)
            .expect("automatic entry should fit in the typed cache");
        assert!(matches!(entered, GameModeTransition::Entered { .. }));
        assert!(controller.is_active());

        let exited = controller
            .reconcile(
                &GameDetector::default().detect(&GameSnapshot::default()),
                original,
                false,
            )
            .expect("cached state should restore");
        assert_eq!(exited, GameModeTransition::Exited { restored: original });
        assert!(!controller.is_active());
        assert_eq!(
            controller
                .reconcile(
                    &GameDetector::default().detect(&GameSnapshot::default()),
                    original,
                    false,
                )
                .expect("second exit is a no-op"),
            GameModeTransition::NoChange { active: false }
        );
    }

    #[test]
    fn manual_mode_never_enters_without_explicit_request() {
        let detection = GameDetector::default().detect(&confirmed_snapshot());
        let mut controller = GameModeController::new(GameModePreference::Manual, PerformanceTier::Balanced);
        assert_eq!(
            controller.reconcile(&detection, ShellState::default(), false),
            Ok(GameModeTransition::NoChange { active: false })
        );
        assert!(matches!(
            controller.reconcile(&detection, ShellState::default(), true),
            Ok(GameModeTransition::Entered { .. })
        ));
    }

    #[test]
    fn disabled_mode_rejects_even_a_direct_enter_request() {
        let mut controller = GameModeController::new(GameModePreference::Disabled, PerformanceTier::Balanced);
        assert_eq!(
            controller.enter(ShellState::default()),
            Err(GameModeError::Disabled)
        );
        assert!(!controller.is_active());
    }

    #[test]
    fn full_cache_fails_safely_without_activating_controller() {
        let mut controller = GameModeController::with_cache_capacity(
            GameModePreference::Manual,
            PerformanceTier::LowEnd,
            CACHED_SHELL_STATE_BYTES - 1,
        );
        let detection = GameDetector::default().detect(&confirmed_snapshot());
        assert_eq!(
            controller.reconcile(&detection, ShellState::default(), true),
            Err(GameModeError::Cache(CacheError::CapacityTooSmall))
        );
        assert!(!controller.is_active());
    }
}
