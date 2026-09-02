//! The Rouch Settings app model: panes, entries and typed values.
//!
//! This is the macOS System Settings structure: a sidebar of categories, a
//! pane of settings with toggles, sliders, selects and action rows, all
//! resolved against the live shell configuration and the hardware backends.

use crate::windowing::{Point, Rect};

/// Width of the settings sidebar.
pub const SIDEBAR_WIDTH: i32 = 210;
pub const SIDEBAR_WIDTH_NARROW: i32 = 128;

/// Height of one sidebar row.
pub const SIDEBAR_ROW: i32 = 34;

/// The window's default rectangle, centred on the work area.
pub const WINDOW_WIDTH: i32 = 880;
pub const WINDOW_HEIGHT: i32 = 560;

/// One settings pane identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    Wifi,
    Bluetooth,
    Appearance,
    Notifications,
    Displays,
    Sound,
    Performance,
    GameMode,
    System,
    Widgets,
    Wallpaper,
    Users,
    Mouse,
    Keyboard,
}

impl Pane {
    /// All panes, in the macOS System Settings order.
    pub const ALL: [Pane; 14] = [
        Pane::Wifi,
        Pane::Bluetooth,
        Pane::Appearance,
        Pane::Notifications,
        Pane::Displays,
        Pane::Sound,
        Pane::Performance,
        Pane::GameMode,
        Pane::System,
        Pane::Widgets,
        Pane::Wallpaper,
        Pane::Users,
        Pane::Mouse,
        Pane::Keyboard,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Pane::Wifi => "Wi-Fi",
            Pane::Bluetooth => "Bluetooth",
            Pane::Appearance => "Appearance",
            Pane::Notifications => "Notifications",
            Pane::Displays => "Displays",
            Pane::Sound => "Sound",
            Pane::Performance => "Performance",
            Pane::GameMode => "Game Mode / Modo Jogo",
            Pane::System => "About",
            Pane::Widgets => "Widgets",
            Pane::Wallpaper => "Wallpaper",
            Pane::Users => "Users & Groups",
            Pane::Mouse => "Mouse",
            Pane::Keyboard => "Keyboard",
        }
    }
}

/// A settings row kind, mirroring the macOS settings vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    /// Non-interactive section heading with one concise explanation.
    Section { title: String, description: String },
    /// Boolean toggle with a description.
    Toggle {
        key: &'static str,
        label: String,
        value: bool,
    },
    /// Slider with range and live value.
    Slider {
        key: &'static str,
        label: String,
        value: f32,
        min: f32,
        max: f32,
    },
    /// Choose one of several options.
    Select {
        key: &'static str,
        label: String,
        options: Vec<String>,
        selected: usize,
    },
    /// A read-only information line.
    Info { label: String, value: String },
    /// A compact status line with a semantic tone and a second line of detail.
    Status {
        label: String,
        value: String,
        detail: String,
        tone: SettingTone,
    },
    /// A button that runs an action.
    Action { key: &'static str, label: String },
    /// The user card in the Users pane.
    UserCard,
}

impl Setting {
    pub fn key(&self) -> Option<&'static str> {
        match self {
            Setting::Toggle { key, .. }
            | Setting::Slider { key, .. }
            | Setting::Select { key, .. }
            | Setting::Action { key, .. } => Some(key),
            _ => None,
        }
    }

    /// Whether this row is a valid keyboard activation target.
    pub const fn is_interactive(&self) -> bool {
        matches!(
            self,
            Setting::Toggle { .. } | Setting::Slider { .. } | Setting::Select { .. } | Setting::Action { .. }
        )
    }
}

/// Semantic colour used by a renderer without coupling the model to pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingTone {
    Neutral,
    Positive,
    Warning,
    Muted,
}

/// The explicit user choice for Game Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GameModeChoice {
    #[default]
    Auto,
    On,
    Off,
}

impl GameModeChoice {
    pub const OPTIONS: [&'static str; 3] = ["Auto / Automatico", "On / Ligado", "Off / Desligado"];

    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::On,
            2 => Self::Off,
            _ => Self::Auto,
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Auto => 0,
            Self::On => 1,
            Self::Off => 2,
        }
    }

    pub const fn config_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// Convert the presentation choice to the pure Game Mode controller.
    pub const fn core_preference(self) -> crate::game_mode::GameModePreference {
        match self {
            Self::Auto => crate::game_mode::GameModePreference::Automatic,
            Self::On => crate::game_mode::GameModePreference::Manual,
            Self::Off => crate::game_mode::GameModePreference::Disabled,
        }
    }

    pub const fn from_core(preference: crate::game_mode::GameModePreference) -> Self {
        match preference {
            crate::game_mode::GameModePreference::Automatic => Self::Auto,
            crate::game_mode::GameModePreference::Manual => Self::On,
            crate::game_mode::GameModePreference::Disabled => Self::Off,
        }
    }
}

/// Automatic Game Mode never acts on a weak signal.
pub const GAME_MODE_AUTO_CONFIDENCE: u8 = 70;

/// Evidence and policy information supplied by the Game Mode core.
///
/// This is deliberately a presentation boundary. It contains no process
/// handles and grants Settings no permission to pause, kill, or reprioritise
/// anything. Until the core is present, the default state says so plainly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameModeSnapshot {
    pub choice: GameModeChoice,
    pub detector_ready: bool,
    pub detection: crate::game_mode::GameDetection,
    pub policy: crate::game_mode::GameModePolicy,
    pub policy_active: bool,
    pub cache_occupied: bool,
    pub cache_capacity_bytes: usize,
    /// Reconciliation failure reported by the compositor adapter, if the last
    /// attempt could not apply the policy. Kept as data so the Settings rows
    /// stay reproducible in tests.
    pub apply_error: Option<String>,
}

impl GameModeSnapshot {
    pub fn unavailable(choice: GameModeChoice) -> Self {
        Self {
            choice,
            detector_ready: false,
            detection: crate::game_mode::GameDetection {
                classification: crate::game_mode::GameClassification::Unknown,
                score: 0,
                confidence_percent: 0,
                target_pid: None,
                independent_evidence: 0,
                reasons: Vec::new(),
            },
            policy: crate::game_mode::GameModePolicy::for_tier(crate::game_mode::PerformanceTier::Balanced),
            policy_active: false,
            cache_occupied: false,
            cache_capacity_bytes: crate::game_mode::MAX_CACHE_BYTES,
            apply_error: None,
        }
    }

    /// Project the detector/controller result into the Settings boundary.
    ///
    /// Settings receives explanations and bounded status strings only; it
    /// never receives process handles, paths, environment values, or a
    /// privilege to alter a process.
    pub fn from_detection(
        detection: &crate::game_mode::GameDetection,
        choice: GameModeChoice,
        policy: crate::game_mode::GameModePolicy,
        policy_active: bool,
        cache_active: bool,
    ) -> Self {
        Self {
            choice,
            detector_ready: true,
            detection: detection.clone(),
            policy,
            policy_active,
            cache_occupied: cache_active,
            cache_capacity_bytes: crate::game_mode::MAX_CACHE_BYTES,
            apply_error: None,
        }
    }

    pub fn detected(&self) -> bool {
        self.detection.classification.is_game()
    }

    pub fn automatic_eligible(&self) -> bool {
        self.detector_ready
            && self.detection.eligible_for_automatic_mode()
            && self.detection.confidence_percent >= GAME_MODE_AUTO_CONFIDENCE
    }

    pub fn detection_label(&self) -> &'static str {
        if !self.detector_ready {
            "Detector indisponivel"
        } else if self.detection.classification == crate::game_mode::GameClassification::ConfirmedGame {
            "Jogo confirmado em primeiro plano"
        } else if self.detected() {
            "Candidato de jogo em primeiro plano"
        } else if self.detection.classification == crate::game_mode::GameClassification::Launcher {
            "Launcher em primeiro plano"
        } else if self.detection.classification == crate::game_mode::GameClassification::CommonApp {
            "App comum em primeiro plano"
        } else {
            "Nenhum jogo confirmado"
        }
    }

    pub fn confidence_label(&self) -> String {
        if !self.detector_ready {
            return "Indisponivel - detector nao conectado".into();
        }
        format!(
            "{}% (score {}; {} familias)",
            self.detection.confidence_percent.min(100),
            self.detection.score,
            self.detection.independent_evidence,
        )
    }

    pub fn reasons_label(&self) -> String {
        if self.detection.reasons.is_empty() {
            return if self.detector_ready {
                "Sem evidencias suficientes; nada sera alterado.".into()
            } else {
                "Aguardando evidencias do nucleo de deteccao.".into()
            };
        }
        self.detection
            .reasons
            .iter()
            .take(crate::game_mode::MAX_EVIDENCE_REASONS)
            .map(|reason| ascii_safe(reason.explanation(), 120))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    pub fn policy_label(&self) -> String {
        if let Some(error) = &self.apply_error {
            return ascii_safe(&format!("Modo Jogo não aplicado: {error}"), 220);
        }
        let effects = self.policy.effects;
        let mut labels = Vec::with_capacity(5);
        if effects.reduce_blur {
            labels.push("blur limitado");
        }
        if effects.reduce_transparency {
            labels.push("transparencia reduzida");
        }
        if effects.pause_background_reads {
            labels.push("leituras pausadas");
        }
        if effects.pause_widget_refresh {
            labels.push("widgets pausados");
        }
        if effects.pause_gallery_refresh {
            labels.push("galeria pausada");
        }
        if effects.limit_animations {
            labels.push("animacoes limitadas");
        }
        if labels.is_empty() {
            return "Nenhuma reducao opcional definida.".into();
        }
        let mut summary = labels.join(", ");
        if let Some(fps) = effects.redraw_cap_fps {
            summary.push_str(&format!("; limite {} FPS", fps));
        }
        if effects.defer_noncritical_notifications {
            summary.push_str("; notificacoes nao criticas em espera");
        }
        summary.push_str("; input/audio/compositor/seguranca preservados");
        ascii_safe(&summary, 220)
    }

    pub fn cache_label(&self) -> String {
        if !self.detector_ready {
            return "Core do cache indisponivel; nenhum estado sera alterado.".into();
        }
        format!(
            "{} / {} bytes; retirada uma vez ao restaurar",
            if self.cache_occupied {
                "1 estado salvo"
            } else {
                "vazio"
            },
            self.cache_capacity_bytes.min(crate::game_mode::MAX_CACHE_BYTES)
        )
    }
}

/// Decide whether the compositor may apply the core-reported policy.
///
/// `manual_request` is intentionally separate from the persisted choice: an
/// On preference arms the mode, but the compositor still needs an explicit
/// action before it can enter. Auto additionally requires the confidence
/// threshold and a core-confirmed active policy.
pub fn game_mode_policy_allowed(snapshot: &GameModeSnapshot, manual_request: bool) -> bool {
    match snapshot.choice {
        GameModeChoice::Auto => snapshot.policy_active && snapshot.automatic_eligible(),
        GameModeChoice::On => {
            manual_request && snapshot.policy_active && snapshot.detector_ready && snapshot.detected()
        }
        GameModeChoice::Off => false,
    }
}

/// Decide whether the explicit Preview action has enough evidence to show the
/// existing visual policy. Preview is never a process-control operation.
pub fn game_mode_preview_allowed(snapshot: &GameModeSnapshot) -> bool {
    match snapshot.choice {
        GameModeChoice::Auto => snapshot.automatic_eligible(),
        GameModeChoice::On => snapshot.detector_ready && snapshot.detected(),
        GameModeChoice::Off => false,
    }
}

fn ascii_safe(text: &str, max_chars: usize) -> String {
    text.chars()
        .take(max_chars)
        .map(|character| if character.is_ascii() { character } else { '?' })
        .collect()
}

/// Keyboard focus target for Settings. Sidebar and content use separate
/// indices so a focus ring never depends on a pixel coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFocus {
    Sidebar(usize),
    Row(usize),
}

/// Find the first useful focus target. The sidebar remains the entry point.
pub fn first_focus() -> SettingsFocus {
    SettingsFocus::Sidebar(0)
}

/// Move focus without allocating a candidate list or making non-interactive
/// rows keyboard stops.
pub fn cycle_focus(current: SettingsFocus, rows: &[Setting], reverse: bool) -> SettingsFocus {
    let pane_count = Pane::ALL.len();
    let interactive_count = rows.iter().filter(|row| row.is_interactive()).count();
    if pane_count == 0 && interactive_count == 0 {
        return current;
    }

    match current {
        SettingsFocus::Sidebar(index) if !reverse => {
            if index + 1 < pane_count {
                SettingsFocus::Sidebar(index + 1)
            } else {
                rows.iter()
                    .position(Setting::is_interactive)
                    .map(SettingsFocus::Row)
                    .unwrap_or(SettingsFocus::Sidebar(0))
            }
        }
        SettingsFocus::Sidebar(index) => {
            if index > 0 {
                SettingsFocus::Sidebar(index - 1)
            } else {
                rows.iter()
                    .rposition(Setting::is_interactive)
                    .map(SettingsFocus::Row)
                    .unwrap_or(SettingsFocus::Sidebar(pane_count.saturating_sub(1)))
            }
        }
        SettingsFocus::Row(index) if !reverse => {
            let next = rows
                .iter()
                .enumerate()
                .skip(index.saturating_add(1))
                .find(|(_, row)| row.is_interactive())
                .map(|(row, _)| row);
            next.map(SettingsFocus::Row).unwrap_or(SettingsFocus::Sidebar(0))
        }
        SettingsFocus::Row(index) => rows
            .iter()
            .enumerate()
            .take(index)
            .rfind(|(_, row)| row.is_interactive())
            .map(|(row, _)| SettingsFocus::Row(row))
            .unwrap_or(SettingsFocus::Sidebar(pane_count.saturating_sub(1))),
    }
}

/// A live, typed value the settings backend reports and accepts.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Float(f32),
    Choice(usize),
    /// A free-form text or path, used by wallpaper picks.
    Text(String),
}

/// The resolved settings window layout.
pub struct SettingsLayout {
    pub window: Rect,
    pub sidebar: Rect,
    pub content: Rect,
    pub rows: Vec<Rect>,
    /// The pane's title band, like macOS's sticky pane header.
    pub title: Rect,
}

/// The window rect centred on the work area.
pub fn window_rect(work_area: Rect) -> Rect {
    let width = WINDOW_WIDTH.min(work_area.size.width.saturating_sub(24).max(1));
    let height = WINDOW_HEIGHT.min(work_area.size.height.saturating_sub(24).max(1));
    Rect::new(
        work_area.origin.x + (work_area.size.width - width).max(0) / 2,
        work_area.origin.y + (work_area.size.height - height).max(0) / 3,
        width,
        height,
    )
}

/// Lay a pane out: sidebar rows and content rows.
pub fn layout(work_area: Rect, pane: Pane, row_count: usize) -> SettingsLayout {
    let window = window_rect(work_area);
    let sidebar_width = if window.size.width < 700 {
        SIDEBAR_WIDTH_NARROW
    } else {
        SIDEBAR_WIDTH
    }
    .min(window.size.width);
    let sidebar = Rect::new(
        window.origin.x,
        window.origin.y + 52,
        sidebar_width,
        (window.size.height - 52).max(0),
    );
    let content = Rect::new(
        sidebar.right(),
        window.origin.y + 52,
        (window.right() - sidebar.right()).max(0),
        (window.size.height - 52).max(0),
    );
    let title = Rect::new(content.origin.x, window.origin.y + 8, content.size.width, 34);

    let row_height = 46;
    let rows = (0..row_count)
        .map(|index| {
            Rect::new(
                content.origin.x + 16,
                content.origin.y + 48 + index as i32 * row_height,
                (content.size.width - 32).max(0),
                row_height - 4,
            )
        })
        .collect();

    let _ = pane;
    SettingsLayout {
        window,
        sidebar,
        content,
        rows,
        title,
    }
}

/// Which sidebar row a point hits, if any.
pub fn sidebar_hit(layout: &SettingsLayout, point: Point) -> Option<Pane> {
    if !layout.sidebar.contains_point(point) {
        return None;
    }
    let index = ((point.y - layout.sidebar.origin.y) / SIDEBAR_ROW) as usize;
    if point.y < layout.sidebar.origin.y {
        return None;
    }
    Pane::ALL.get(index).copied()
}

/// Which content row a point hits, if any.
pub fn row_hit(layout: &SettingsLayout, point: Point) -> Option<usize> {
    layout.rows.iter().position(|row| row.contains_point(point))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

    #[test]
    fn window_is_centred_and_fits() {
        let window = window_rect(WORK_AREA);
        assert!(window.right() <= WORK_AREA.right());
        assert!(window.bottom() <= WORK_AREA.bottom());
        let centre = window.origin.x + window.size.width / 2;
        assert_eq!(centre, WORK_AREA.size.width / 2);
    }

    #[test]
    fn sidebar_lists_every_pane_in_order() {
        let layout = layout(WORK_AREA, Pane::System, 3);
        assert_eq!(layout.sidebar.size.width, SIDEBAR_WIDTH);

        let first = Point {
            x: layout.sidebar.origin.x + 10,
            y: layout.sidebar.origin.y + 10,
        };
        assert_eq!(sidebar_hit(&layout, first), Some(Pane::Wifi));

        let second = Point {
            x: layout.sidebar.origin.x + 10,
            y: layout.sidebar.origin.y + SIDEBAR_ROW + 10,
        };
        assert_eq!(sidebar_hit(&layout, second), Some(Pane::Bluetooth));

        let outside = Point {
            x: layout.sidebar.right() + 20,
            y: layout.sidebar.origin.y + 10,
        };
        assert_eq!(sidebar_hit(&layout, outside), None);
    }

    #[test]
    fn content_rows_hit_in_order() {
        let layout = layout(WORK_AREA, Pane::Sound, 4);
        let second = Point {
            x: layout.rows[1].origin.x + 5,
            y: layout.rows[1].origin.y + 5,
        };
        assert_eq!(row_hit(&layout, second), Some(1));
    }

    #[test]
    fn narrow_settings_keeps_both_navigation_and_content_visible() {
        let layout = layout(Rect::new(0, 0, 390, 700), Pane::GameMode, 8);
        assert_eq!(layout.window.size.width, 366);
        assert_eq!(layout.sidebar.size.width, SIDEBAR_WIDTH_NARROW);
        assert!(layout.content.size.width > 0);
        assert!(layout.rows.iter().all(|row| row.size.width >= 0));
    }

    #[test]
    fn settings_carry_keys_for_backend_routing() {
        let toggle = Setting::Toggle {
            key: "wifi.enabled",
            label: "Wi-Fi".into(),
            value: true,
        };
        assert_eq!(toggle.key(), Some("wifi.enabled"));
        let info = Setting::Info {
            label: "Chip".into(),
            value: "Test".into(),
        };
        assert_eq!(info.key(), None);
    }

    #[test]
    fn notification_and_performance_panes_are_first_class() {
        assert!(Pane::ALL.contains(&Pane::Notifications));
        assert!(Pane::ALL.contains(&Pane::Performance));
        assert_eq!(Pane::Notifications.title(), "Notifications");
        assert_eq!(Pane::Performance.title(), "Performance");
        assert!(Pane::ALL.contains(&Pane::GameMode));
        assert_eq!(Pane::GameMode.title(), "Game Mode / Modo Jogo");
    }

    #[test]
    fn game_mode_requires_trusted_confidence_for_auto() {
        let mut snapshot = GameModeSnapshot::unavailable(GameModeChoice::Auto);
        assert!(!snapshot.automatic_eligible());

        snapshot.detector_ready = true;
        snapshot.detection.classification = crate::game_mode::GameClassification::ConfirmedGame;
        snapshot.detection.confidence_percent = GAME_MODE_AUTO_CONFIDENCE - 1;
        assert!(!snapshot.automatic_eligible());

        snapshot.detection.confidence_percent = GAME_MODE_AUTO_CONFIDENCE;
        assert!(snapshot.automatic_eligible());
        snapshot.policy_active = true;
        assert!(game_mode_policy_allowed(&snapshot, false));
        assert!(game_mode_policy_allowed(&snapshot, true));
    }

    #[test]
    fn game_mode_preview_is_explicit_and_never_uses_a_name_as_proof() {
        let mut snapshot = GameModeSnapshot::unavailable(GameModeChoice::On);
        snapshot.detection.classification = crate::game_mode::GameClassification::ProbableGame;
        assert!(!game_mode_preview_allowed(&snapshot));
        snapshot.detector_ready = true;
        snapshot.detection.classification = crate::game_mode::GameClassification::ConfirmedGame;
        assert!(game_mode_preview_allowed(&snapshot));
        snapshot.policy_active = true;
        assert!(!game_mode_policy_allowed(&snapshot, false));
        assert!(game_mode_policy_allowed(&snapshot, true));
    }

    #[test]
    fn game_mode_rows_keep_evidence_separate_from_detection_state() {
        let snapshot = GameModeSnapshot {
            choice: GameModeChoice::Auto,
            detector_ready: true,
            detection: crate::game_mode::GameDetection {
                classification: crate::game_mode::GameClassification::ConfirmedGame,
                score: 84,
                confidence_percent: 84,
                target_pid: Some(42),
                independent_evidence: 3,
                reasons: vec![crate::game_mode::EvidenceReason {
                    code: crate::game_mode::EvidenceCode::ProtonRuntime,
                    points: 34,
                    origin: crate::game_mode::EvidenceOrigin::Process { pid: 42 },
                }],
            },
            policy: crate::game_mode::GameModePolicy::for_tier(crate::game_mode::PerformanceTier::Balanced),
            policy_active: true,
            cache_occupied: true,
            cache_capacity_bytes: crate::game_mode::MAX_CACHE_BYTES,
            apply_error: None,
        };
        assert_eq!(snapshot.detection_label(), "Jogo confirmado em primeiro plano");
        assert!(snapshot.reasons_label().contains("Proton"));
        assert!(snapshot.confidence_label().contains("84%"));
        assert!(snapshot.policy_label().contains("blur"));
        assert!(snapshot.cache_label().contains("1 estado"));
    }

    #[test]
    fn settings_focus_skips_status_and_info_rows() {
        let rows = vec![
            Setting::Section {
                title: "Game Mode".into(),
                description: "Description".into(),
            },
            Setting::Status {
                label: "Detection".into(),
                value: "None".into(),
                detail: "0%".into(),
                tone: SettingTone::Muted,
            },
            Setting::Info {
                label: "Evidence".into(),
                value: "None".into(),
            },
            Setting::Select {
                key: "game_mode.mode",
                label: "Mode".into(),
                options: GameModeChoice::OPTIONS
                    .iter()
                    .map(|label| (*label).into())
                    .collect(),
                selected: 0,
            },
            Setting::Action {
                key: "game_mode.preview",
                label: "Preview".into(),
            },
        ];
        assert_eq!(
            cycle_focus(SettingsFocus::Sidebar(Pane::ALL.len() - 1), &rows, false),
            SettingsFocus::Row(3)
        );
        assert_eq!(
            cycle_focus(SettingsFocus::Row(3), &rows, false),
            SettingsFocus::Row(4)
        );
        assert_eq!(
            cycle_focus(SettingsFocus::Row(4), &rows, true),
            SettingsFocus::Row(3)
        );
    }
}
