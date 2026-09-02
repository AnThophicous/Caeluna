//! The real settings backend: every pane reads and writes the live system.
//!
//! Network state comes from the readings cache, radios switch through
//! `nmcli`/`bluetoothctl`, brightness and volume write to their subsystems,
//! and appearance-like preferences persist under `~/.config/rouch`.

use std::path::PathBuf;
use std::process::Command;

use crate::settings::{GameModeChoice, GameModeSnapshot, Pane, Setting, SettingTone, SettingValue};
use crate::topbar::Distribution;
use crate::user::{AvatarMark, UserProfile};

use super::backends::SystemReadings;
use super::shell_render::ShellToggles;

/// The settings application state, backed by the real system.
pub struct SettingsBackend {
    pub readings_cache: SystemReadings,
    game_mode: GameModeSnapshot,
}

impl Default for SettingsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsBackend {
    pub fn new() -> Self {
        let choice = game_mode_choice();
        Self {
            readings_cache: SystemReadings::read(),
            game_mode: GameModeSnapshot::unavailable(choice),
        }
    }

    /// The last evidence snapshot supplied by the Game Mode core. Settings
    /// only presents this data; it does not discover, pause, or kill a
    /// process by itself.
    pub fn game_mode_snapshot(&self) -> &GameModeSnapshot {
        &self.game_mode
    }

    /// Replace the presentation snapshot at a compositor-owned boundary.
    /// The caller is responsible for obtaining it from the real Game Mode
    /// detector and for applying its policy conservatively.
    pub fn set_game_mode_snapshot(&mut self, snapshot: GameModeSnapshot) {
        self.game_mode = snapshot;
    }

    /// The rows of one pane, resolved against the live system.
    pub fn pane_rows(&self, pane: Pane, profile: &UserProfile, toggles: &ShellToggles) -> Vec<Setting> {
        match pane {
            Pane::Wifi => self.wifi_rows(),
            Pane::Bluetooth => self.bluetooth_rows(),
            Pane::Appearance => self.appearance_rows(toggles),
            Pane::Notifications => self.notifications_rows(),
            Pane::Displays => self.displays_rows(toggles),
            Pane::Sound => self.sound_rows(toggles),
            Pane::Performance => self.performance_rows(),
            Pane::GameMode => self.game_mode_rows(),
            Pane::System => self.system_rows(profile),
            Pane::Widgets => self.widgets_rows(),
            Pane::Wallpaper => self.wallpaper_rows(),
            Pane::Users => self.users_rows(profile),
            Pane::Mouse => self.mouse_rows(),
            Pane::Keyboard => self.keyboard_rows(),
        }
    }

    fn wifi_rows(&self) -> Vec<Setting> {
        let connected = self
            .readings_cache
            .network_name
            .clone()
            .unwrap_or_else(|| "Off".to_owned());
        vec![
            Setting::Info {
                label: "Network".into(),
                value: connected,
            },
            Setting::Info {
                label: "Interface".into(),
                value: wifi_interface().unwrap_or_else(|| "none".into()),
            },
            Setting::Action {
                key: "turn_on",
                label: "Turn Wi-Fi On".into(),
            },
            Setting::Action {
                key: "turn_off",
                label: "Turn Wi-Fi Off".into(),
            },
        ]
    }

    fn bluetooth_rows(&self) -> Vec<Setting> {
        let mut rows = vec![
            Setting::Info {
                label: "Powered".into(),
                value: if bluetooth_powered() {
                    "Yes".into()
                } else {
                    "No".into()
                },
            },
            Setting::Action {
                key: "turn_on",
                label: "Turn Bluetooth On".into(),
            },
            Setting::Action {
                key: "turn_off",
                label: "Turn Bluetooth Off".into(),
            },
        ];
        for device in bluetooth_devices() {
            rows.push(Setting::Info {
                label: device,
                value: "Paired".into(),
            });
        }
        rows
    }

    fn appearance_rows(&self, toggles: &ShellToggles) -> Vec<Setting> {
        vec![
            Setting::Toggle {
                key: "dark_mode",
                label: "Dark Mode".into(),
                value: toggles.dark_mode,
            },
            Setting::Toggle {
                key: "focus",
                label: "Focus".into(),
                value: toggles.focus,
            },
            Setting::Toggle {
                key: "battery_saver",
                label: "Battery Saver".into(),
                value: toggles.battery_saver,
            },
            Setting::Select {
                key: "accent",
                label: "Accent".into(),
                options: ["Ocean", "Sky", "Meadow", "Ember", "Frost"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                selected: read_config("appearance").parse().unwrap_or(0),
            },
        ]
    }

    fn notifications_rows(&self) -> Vec<Setting> {
        vec![
            Setting::Toggle {
                key: "notifications.enabled",
                label: "Allow Notifications".into(),
                value: config_bool("notifications.enabled", true),
            },
            Setting::Toggle {
                key: "notifications.dnd",
                label: "Do Not Disturb".into(),
                value: config_bool("notifications.dnd", false),
            },
            Setting::Toggle {
                key: "notifications.sounds",
                label: "Notification Sounds".into(),
                value: config_bool("notifications.sounds", true),
            },
            Setting::Select {
                key: "notifications.position",
                label: "Delivery Position".into(),
                options: ["Top Right", "Bottom Right"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                selected: config_choice("notifications.position", 0),
            },
            Setting::Action {
                key: "notifications.clear",
                label: "Clear Notification History".into(),
            },
        ]
    }

    fn displays_rows(&self, toggles: &ShellToggles) -> Vec<Setting> {
        vec![
            Setting::Info {
                label: "Resolution".into(),
                value: display_resolution().unwrap_or_else(|| "unknown".into()),
            },
            Setting::Slider {
                key: "brightness",
                label: "Brightness".into(),
                value: toggles.brightness,
                min: 0.0,
                max: 1.0,
            },
        ]
    }

    fn sound_rows(&self, toggles: &ShellToggles) -> Vec<Setting> {
        vec![
            Setting::Info {
                label: "Output".into(),
                value: default_sink().unwrap_or_else(|| "unknown".into()),
            },
            Setting::Slider {
                key: "volume",
                label: "Volume".into(),
                value: toggles.volume,
                min: 0.0,
                max: 1.0,
            },
            Setting::Action {
                key: "mute",
                label: "Toggle Mute".into(),
            },
        ]
    }

    fn performance_rows(&self) -> Vec<Setting> {
        // Smithay 0.7's Winit backend owns an EGL surface, so this is the
        // renderer actually active in the current graphical path. Vulkan is
        // still the persisted preference for the native backend/future Winit
        // integration and remains the first policy candidate.
        let backend = "OpenGL (nested Winit)";
        vec![
            Setting::Info {
                label: "Active Renderer".into(),
                value: backend.into(),
            },
            Setting::Select {
                key: "graphics.backend",
                label: "Renderer Preference".into(),
                options: ["Vulkan (preferred)", "OpenGL (fallback)", "Opaque fallback"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                selected: config_choice("graphics.backend", 0),
            },
            Setting::Toggle {
                key: "reduce_transparency",
                label: "Reduce Transparency".into(),
                value: config_bool("reduce_transparency", false),
            },
            Setting::Info {
                label: "Swapfile".into(),
                value: swap_status(),
            },
            Setting::Info {
                label: "Recommended".into(),
                value: recommended_swap_status(),
            },
            Setting::Action {
                key: "swap.recommend",
                label: "Review Swapfile Plan".into(),
            },
        ]
    }

    fn game_mode_rows(&self) -> Vec<Setting> {
        let snapshot = &self.game_mode;
        let tone = if !snapshot.detector_ready {
            SettingTone::Muted
        } else if snapshot.automatic_eligible() || snapshot.policy_active {
            SettingTone::Positive
        } else if snapshot.detected() {
            SettingTone::Warning
        } else {
            SettingTone::Neutral
        };
        let policy = if snapshot.policy_active {
            snapshot.policy_label()
        } else {
            format!("Preview: {}", snapshot.policy_label())
        };

        vec![
            Setting::Section {
                title: "Game Mode / Modo Jogo".into(),
                description: "Reduz trabalho nao essencial do jogo em primeiro plano; entrada, audio, seguranca e compositor continuam ativos.".into(),
            },
            Setting::Select {
                key: "game_mode.mode",
                label: "Mode / Modo".into(),
                options: GameModeChoice::OPTIONS.iter().map(|label| (*label).into()).collect(),
                selected: snapshot.choice.index(),
            },
            Setting::Status {
                label: "Current detection / Deteccao atual".into(),
                value: snapshot.detection_label().into(),
                detail: format!("Confianca: {}", snapshot.confidence_label()),
                tone,
            },
            Setting::Info {
                label: "Evidence / Evidencias".into(),
                value: snapshot.reasons_label(),
            },
            Setting::Info {
                label: "Policy / Politica".into(),
                value: policy,
            },
            Setting::Action {
                key: "game_mode.preview",
                label: "Preview policy / Previa".into(),
            },
            Setting::Action {
                key: "game_mode.restore",
                label: "Restore / Restaurar".into(),
            },
            Setting::Info {
                label: "Temporary cache / Cache temporario".into(),
                value: snapshot.cache_label(),
            },
        ]
    }

    fn system_rows(&self, profile: &UserProfile) -> Vec<Setting> {
        vec![
            Setting::Info {
                label: "User".into(),
                value: profile.name.clone(),
            },
            Setting::Info {
                label: "Host".into(),
                value: self.readings_cache.hostname.clone(),
            },
            Setting::Info {
                label: "Distribution".into(),
                value: self.readings_cache.distribution.name().to_owned(),
            },
            Setting::Info {
                label: "Kernel".into(),
                value: kernel_version(),
            },
            Setting::Info {
                label: "CPU".into(),
                value: cpu_model(),
            },
            Setting::Info {
                label: "Memory".into(),
                value: total_memory(),
            },
        ]
    }

    fn widgets_rows(&self) -> Vec<Setting> {
        vec![
            Setting::Select {
                key: "layout",
                label: "Layout".into(),
                options: ["Rail", "Hidden"].iter().map(|s| s.to_string()).collect(),
                selected: read_config("widgets").parse().unwrap_or(0),
            },
            Setting::Action {
                key: "reset_board",
                label: "Reset Widget Board".into(),
            },
        ]
    }

    fn wallpaper_rows(&self) -> Vec<Setting> {
        vec![
            Setting::Select {
                key: "source",
                label: "Source".into(),
                options: ["Wallpaper.webp", "Solid", "None"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                selected: read_config("wallpaper").parse().unwrap_or(0),
            },
            Setting::Action {
                key: "pick_image",
                label: "Pick Image...".into(),
            },
        ]
    }

    fn users_rows(&self, profile: &UserProfile) -> Vec<Setting> {
        vec![
            Setting::UserCard,
            Setting::Info {
                label: "Name".into(),
                value: profile.name.clone(),
            },
            Setting::Select {
                key: "mark",
                label: "Avatar Mark".into(),
                options: AvatarMark::ALL.iter().map(|mark| format!("{mark:?}")).collect(),
                selected: read_config("avatar").parse().unwrap_or(0),
            },
            Setting::Action {
                key: "change_avatar",
                label: "Change Avatar...".into(),
            },
        ]
    }

    fn mouse_rows(&self) -> Vec<Setting> {
        vec![
            Setting::Slider {
                key: "speed",
                label: "Tracking Speed".into(),
                value: read_config("mouse").parse().unwrap_or(0.5),
                min: 0.0,
                max: 1.0,
            },
            Setting::Toggle {
                key: "natural_scroll",
                label: "Natural Scrolling".into(),
                value: read_config("natural").parse().unwrap_or(false),
            },
            Setting::Info {
                label: "Devices".into(),
                value: input_devices("Mouse").join(", "),
            },
        ]
    }

    fn keyboard_rows(&self) -> Vec<Setting> {
        vec![
            Setting::Info {
                label: "Layout".into(),
                value: keyboard_layout(),
            },
            Setting::Toggle {
                key: "repeat_keys",
                label: "Key Repeat".into(),
                value: read_config("repeat").parse().unwrap_or(true),
            },
            Setting::Info {
                label: "Devices".into(),
                value: input_devices("Keyboard").join(", "),
            },
        ]
    }

    /// Apply one setting change to the real system.
    pub fn apply(&mut self, pane: Pane, key: &str, value: SettingValue) -> Option<SettingValue> {
        match (pane, key, value) {
            (Pane::Wifi, "turn_on", _) => {
                run("nmcli", &["radio", "wifi", "on"]);
                Some(SettingValue::Bool(true))
            }
            (Pane::Wifi, "turn_off", _) => {
                run("nmcli", &["radio", "wifi", "off"]);
                Some(SettingValue::Bool(false))
            }
            (Pane::Bluetooth, "turn_on", _) => {
                run("bluetoothctl", &["power", "on"]);
                Some(SettingValue::Bool(true))
            }
            (Pane::Bluetooth, "turn_off", _) => {
                run("bluetoothctl", &["power", "off"]);
                Some(SettingValue::Bool(false))
            }
            (Pane::Appearance, "dark_mode", SettingValue::Bool(on)) => {
                write_config("dark_mode", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            (Pane::Appearance, "focus", SettingValue::Bool(on)) => {
                write_config("focus", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            (Pane::Appearance, "battery_saver", SettingValue::Bool(on)) => {
                write_config("battery_saver", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            (Pane::Appearance, "accent", SettingValue::Choice(index)) => {
                write_config("appearance", &index.to_string());
                Some(SettingValue::Choice(index))
            }
            (Pane::Notifications, "notifications.enabled", SettingValue::Bool(on)) => {
                write_config("notifications.enabled", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            (Pane::Notifications, "notifications.dnd", SettingValue::Bool(on)) => {
                write_config("notifications.dnd", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            (Pane::Notifications, "notifications.sounds", SettingValue::Bool(on)) => {
                write_config("notifications.sounds", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            (Pane::Notifications, "notifications.position", SettingValue::Choice(index)) => {
                write_config("notifications.position", &index.to_string());
                Some(SettingValue::Choice(index))
            }
            (Pane::Notifications, "notifications.clear", _) => Some(SettingValue::Bool(true)),
            (Pane::Displays, "brightness", SettingValue::Float(level)) => {
                set_brightness(level);
                Some(SettingValue::Float(level))
            }
            (Pane::Sound, "volume", SettingValue::Float(level)) => {
                set_volume(level);
                Some(SettingValue::Float(level))
            }
            (Pane::Sound, "mute", _) => {
                run("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "toggle"]);
                Some(SettingValue::Bool(true))
            }
            (Pane::Performance, "graphics.backend", SettingValue::Choice(index)) => {
                write_config("graphics.backend", &index.to_string());
                Some(SettingValue::Choice(index))
            }
            (Pane::Performance, "reduce_transparency", SettingValue::Bool(on)) => {
                write_config("reduce_transparency", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            (Pane::Performance, "swap.recommend", _) => Some(SettingValue::Bool(true)),
            (Pane::GameMode, "game_mode.mode", SettingValue::Choice(index)) => {
                let choice = GameModeChoice::from_index(index);
                write_config("game_mode.mode", choice.config_value());
                self.game_mode.choice = choice;
                Some(SettingValue::Choice(choice.index()))
            }
            (Pane::GameMode, "game_mode.preview", _) => Some(SettingValue::Bool(true)),
            (Pane::GameMode, "game_mode.restore", _) => Some(SettingValue::Bool(true)),
            (Pane::Widgets, "layout", SettingValue::Choice(index)) => {
                write_config("widgets", &index.to_string());
                Some(SettingValue::Choice(index))
            }
            (Pane::Widgets, "reset_board", _) => Some(SettingValue::Bool(true)),
            (Pane::Wallpaper, "source", SettingValue::Choice(index)) => {
                write_config("wallpaper", &index.to_string());
                Some(SettingValue::Choice(index))
            }
            (Pane::Users, "mark", SettingValue::Choice(index)) => {
                write_config("avatar", &index.to_string());
                Some(SettingValue::Choice(index))
            }
            (Pane::Mouse, "speed", SettingValue::Float(speed)) => {
                write_config("mouse", &speed.to_string());
                Some(SettingValue::Float(speed))
            }
            (Pane::Mouse, "natural_scroll", SettingValue::Bool(on)) => {
                write_config("natural", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            (Pane::Keyboard, "repeat_keys", SettingValue::Bool(on)) => {
                write_config("repeat", &on.to_string());
                Some(SettingValue::Bool(on))
            }
            _ => None,
        }
    }

    /// Build a read-only, human-readable swap recommendation for the shell
    /// notification. Planning never invokes a privileged command.
    pub fn swap_recommendation(&self) -> String {
        let Some(memory_bytes) = total_memory_bytes() else {
            return "Could not read physical memory; no swapfile change was planned.".into();
        };
        let Ok(size_bytes) = crate::swapfile::recommended_swap_size(memory_bytes) else {
            return "Physical memory was outside the safe swap recommendation range.".into();
        };
        let active = std::fs::read_to_string("/proc/swaps")
            .ok()
            .and_then(|body| crate::swapfile::parse_proc_swaps(&body).ok())
            .map(|entries| entries.len())
            .unwrap_or(0);
        format!(
            "Recommended {} at {}. {} active swap device(s); preview the plan before any privileged change.",
            format_bytes(size_bytes),
            crate::swapfile::DEFAULT_SWAPFILE_PATH,
            active
        )
    }
}

fn game_mode_choice() -> GameModeChoice {
    match read_config("game_mode.mode").trim() {
        "on" | "1" | "true" => GameModeChoice::On,
        "off" | "2" | "false" => GameModeChoice::Off,
        _ => GameModeChoice::Auto,
    }
}

/// The Rouch config directory, created on demand.
fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(home).join(".config").join("rouch");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Read one persisted config value; "0" when absent.
fn read_config(key: &str) -> String {
    std::fs::read_to_string(config_dir().join(key)).unwrap_or_else(|_| "0".into())
}

fn config_bool(key: &str, default: bool) -> bool {
    match read_config(key).trim() {
        "1" | "true" | "on" | "yes" => true,
        "0" | "false" | "off" | "no" => false,
        _ => default,
    }
}

fn config_choice(key: &str, default: usize) -> usize {
    read_config(key).trim().parse().unwrap_or(default)
}

fn total_memory_bytes() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find(|line| line.starts_with("MemTotal"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(crate::swapfile::KIBIBYTE))
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= crate::swapfile::GIBIBYTE {
        format!("{:.1} GiB", bytes as f64 / crate::swapfile::GIBIBYTE as f64)
    } else {
        format!("{} MiB", bytes / crate::swapfile::MEBIBYTE)
    }
}

fn swap_status() -> String {
    let Some(body) = std::fs::read_to_string("/proc/swaps").ok() else {
        return "Unavailable".into();
    };
    let entries = crate::swapfile::parse_proc_swaps(&body).unwrap_or_default();
    if entries.is_empty() {
        "Not active".into()
    } else {
        format!("{} active", entries.len())
    }
}

fn recommended_swap_status() -> String {
    total_memory_bytes()
        .and_then(|memory| crate::swapfile::recommended_swap_size(memory).ok())
        .map(format_bytes)
        .unwrap_or_else(|| "Unavailable".into())
}

/// Persist one config value under its key.
fn write_config(key: &str, value: &str) {
    let _ = std::fs::write(config_dir().join(key), value);
}

/// Run a helper command, ignoring whether it exists.
fn run(program: &str, args: &[&str]) {
    let _ = Command::new(program).args(args).status();
}

/// The first non-loopback wireless-capable interface.
fn wifi_interface() -> Option<String> {
    std::fs::read_dir("/sys/class/net")
        .ok()?
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name != "lo" && std::fs::read_to_string(entry.path().join("operstate")).is_ok()
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
}

/// Whether the Bluetooth radio is on.
fn bluetooth_powered() -> bool {
    Command::new("bluetoothctl")
        .arg("show")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("Powered: yes"))
        .unwrap_or(false)
}

/// The paired Bluetooth devices, from `bluetoothctl devices`.
fn bluetooth_devices() -> Vec<String> {
    Command::new("bluetoothctl")
        .arg("devices")
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.strip_prefix("Device ").map(|rest| rest.to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

/// The preferred display mode, from the first DRM card with modes.
fn display_resolution() -> Option<String> {
    for card in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let modes = std::fs::read_to_string(card.path().join("modes")).ok()?;
        if let Some(first) = modes.lines().next() {
            return Some(first.to_owned());
        }
    }
    None
}

/// The default PulseAudio/PipeWire sink name.
fn default_sink() -> Option<String> {
    Command::new("pactl")
        .args(["get-default-sink"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Set the output volume through pactl.
fn set_volume(level: f32) {
    let percent = (level.clamp(0.0, 1.0) * 100.0).round() as i32;
    run(
        "pactl",
        &["set-sink-volume", "@DEFAULT_SINK@", &format!("{percent}%")],
    );
}

/// Set the backlight level, directly when possible, brightnessctl otherwise.
fn set_brightness(level: f32) {
    let level = level.clamp(0.0, 1.0);
    if let Some(dir) = std::fs::read_dir("/sys/class/backlight")
        .ok()
        .and_then(|entries| entries.flatten().next().map(|entry| entry.path()))
    {
        let max: i32 = std::fs::read_to_string(dir.join("max_brightness"))
            .ok()
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or(1);
        let value = (level * max as f32).round() as i32;
        if std::fs::write(dir.join("brightness"), value.to_string()).is_ok() {
            return;
        }
    }
    let percent = (level * 100.0).round() as i32;
    run("brightnessctl", &["set", &format!("{percent}%")]);
}

/// The running kernel version.
fn kernel_version() -> String {
    Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

/// The first CPU model name.
fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|cpuinfo| {
            cpuinfo
                .lines()
                .find(|line| line.starts_with("model name"))
                .and_then(|line| line.split(':').nth(1))
                .map(|model| model.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Total memory, human-readable.
fn total_memory() -> String {
    let kb: f64 = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| {
            meminfo
                .lines()
                .find(|line| line.starts_with("MemTotal"))
                .and_then(|line| line.split_whitespace().nth(1).and_then(|kb| kb.parse().ok()))
        })
        .unwrap_or(0.0);
    let gb = kb / (1024.0 * 1024.0);
    format!("{gb:.1} GB")
}

/// The active keyboard layout, from setxkbmap when available.
fn keyboard_layout() -> String {
    Command::new("setxkbmap")
        .arg("-query")
        .output()
        .ok()
        .and_then(|out| {
            let text: String = String::from_utf8_lossy(&out.stdout).into_owned();
            text.lines()
                .find(|line| line.starts_with("layout"))
                .and_then(|line| line.split(':').nth(1))
                .map(|layout| layout.trim().to_owned())
        })
        .unwrap_or_else(|| "us".into())
}

/// Input device names of one kind, from /proc/bus/input/devices.
fn input_devices(kind: &str) -> Vec<String> {
    std::fs::read_to_string("/proc/bus/input/devices")
        .map(|devices| {
            devices
                .split("\n\n")
                .filter(|block| block.contains(kind))
                .filter_map(|block| {
                    block
                        .lines()
                        .find(|line| line.starts_with("N: Name="))
                        .map(|line| line.split('=').nth(1).unwrap_or("").trim_matches('"').to_owned())
                })
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The current distribution, for rows that name it.
#[allow(dead_code)]
fn distribution_name(readings: &SystemReadings) -> &'static str {
    let _ = Distribution::Unknown.name();
    readings.distribution.name()
}
