//! Real hardware readings for widgets and the control centre.
//!
//! Every source is plain `std::fs` over `/sys` and `/proc`, plus two
//! well-known user-space tools (`pactl`, `df`) where no universal kernel
//! interface exists:
//!
//! * Battery — `/sys/class/power_supply/BAT*/capacity` and `status`.
//! * Brightness — `/sys/class/backlight/*/brightness` over `max_brightness`.
//! * Volume — `pactl get-sink-volume @DEFAULT_SINK@` against the running
//!   PipeWire/PulseAudio daemon.
//! * Network — `/proc/net/wireless`, falling back to the sysfs `operstate`
//!   of any non-loopback interface.
//! * CPU — successive non-blocking `/proc/stat` samples.
//! * Memory — `MemTotal`/`MemAvailable` from `/proc/meminfo`.
//! * Storage — `df -P /` for the root filesystem's used percent.
//! * Hostname — `/etc/hostname`.
//! * Distribution — `/etc/os-release`, parsed by
//!   [`crate::topbar::Distribution::detect`].
//!
//! Nothing here panics: a missing battery, a silent audio stack or an
//! unreadable sysfs node simply degrades to `None` or `0.0`.

// The widget and control-centre renderers sample this module from the UI
// milestone; it ships compiled ahead of its callers.
#![allow(dead_code)]

use std::{
    fs,
    path::Path,
    process::Command,
    sync::{Mutex, Once, OnceLock},
    time::{Duration, Instant},
};

use tracing::{info, warn};

/// The previous aggregate CPU sample. Sampling is intentionally non-blocking:
/// sleeping on the compositor thread for a second sample caused a visible
/// frame stall every time the widget refresh ran.
static PREVIOUS_CPU_TOTALS: OnceLock<Mutex<Option<(f32, f32)>>> = OnceLock::new();
static RENDERER_POLICY_ANNOUNCED: Once = Once::new();

/// External command readings are intentionally a little slower than the
/// widget refresh cadence. This avoids spawning `pactl` and `df` in the frame
/// loop every 750 ms while keeping their UI values near-live.
const COMMAND_READING_TTL: Duration = Duration::from_secs(2);

struct CachedReading<T> {
    value: T,
    refreshed_at: Option<Instant>,
}

static VOLUME_READING: OnceLock<Mutex<CachedReading<Option<f32>>>> = OnceLock::new();
static STORAGE_READING: OnceLock<Mutex<CachedReading<f32>>> = OnceLock::new();

/// Report the deterministic startup decision once. The current nested output
/// exposes OpenGL/EGL and is not a Vulkan-capable Winit surface yet, so the
/// shared Vulkan-first selector records the safe fallback without probing or
/// touching the render loop on every frame.
fn announce_renderer_policy_once() {
    RENDERER_POLICY_ANNOUNCED.call_once(|| {
        let decision = crate::graphics::select_from_availability(false, true);
        info!(
            selected = ?decision.backend,
            fallback = decision.fallback_used,
            performance = ?decision.performance,
            diagnostics = ?decision.diagnostics,
            "Rouch renderer policy"
        );
    });
}

/// One complete hardware snapshot. Read it with [`SystemReadings::read`],
/// then project individual widgets through [`SystemReadings::widget`].
#[derive(Debug, Clone)]
pub struct SystemReadings {
    /// Battery charge percent, or `None` on desktops.
    pub battery_percent: Option<f32>,
    pub battery_charging: bool,
    /// Screen brightness as a 0.0..1.0 fraction, when a backlight exists.
    pub brightness: Option<f32>,
    /// Default sink volume as a 0.0..1.0 fraction, when `pactl` answers.
    pub volume: Option<f32>,
    pub network_name: Option<String>,
    pub network_up: bool,
    pub cpu_percent: f32,
    pub memory_percent: f32,
    pub storage_percent: f32,
    pub hostname: String,
    pub distribution: crate::topbar::Distribution,
}

impl SystemReadings {
    /// Read everything the hardware offers. Never panics; missing values
    /// become `None` and failed math becomes `0.0`.
    pub fn read() -> Self {
        // The policy log is one-shot and explains why this build currently
        // chooses the working OpenGL path when Auto prefers Vulkan.
        announce_renderer_policy_once();
        let (battery_percent, battery_charging) = battery();
        let (network_name, network_up) = network();

        Self {
            battery_percent,
            battery_charging,
            brightness: brightness(),
            volume: volume(),
            network_name,
            network_up,
            cpu_percent: cpu_percent(),
            memory_percent: memory_percent(),
            storage_percent: storage_percent(),
            hostname: hostname(),
            distribution: crate::topbar::Distribution::detect(
                &fs::read_to_string("/etc/os-release").unwrap_or_default(),
            ),
        }
    }

    /// Project one widget's reading out of the snapshot.
    pub fn widget(&self, kind: crate::widgets::WidgetKind) -> crate::widgets::WidgetReading {
        use crate::widgets::WidgetKind;

        match kind {
            WidgetKind::Battery => match self.battery_percent {
                Some(percent) => crate::widgets::WidgetReading::percent(
                    percent,
                    if self.battery_charging {
                        "Charging"
                    } else {
                        "On Battery"
                    },
                ),
                None => crate::widgets::WidgetReading::captioned("No Battery"),
            },
            WidgetKind::Brightness => {
                crate::widgets::WidgetReading::percent(self.brightness.unwrap_or(0.0) * 100.0, "Screen")
            }
            WidgetKind::Volume => {
                crate::widgets::WidgetReading::percent(self.volume.unwrap_or(0.0) * 100.0, "Output")
            }
            WidgetKind::Network => {
                crate::widgets::WidgetReading::captioned(self.network_name.as_deref().unwrap_or("Offline"))
            }
            WidgetKind::Clock => crate::widgets::WidgetReading::captioned(""),
            WidgetKind::Storage => crate::widgets::WidgetReading::percent(self.storage_percent, "Disk"),
            WidgetKind::Cpu => crate::widgets::WidgetReading::percent(self.cpu_percent, "CPU"),
            WidgetKind::Memory => crate::widgets::WidgetReading::percent(self.memory_percent, "RAM"),
        }
    }
}

/// Battery percent and charging flag from the first `BAT*` entry under
/// `/sys/class/power_supply`, or `(None, false)` when absent.
fn battery() -> (Option<f32>, bool) {
    let Some(supply) = first_entry("/sys/class/power_supply", "BAT") else {
        return (None, false);
    };

    let percent = read_trimmed(&supply.join("capacity"))
        .and_then(|raw| raw.parse::<u32>().ok())
        .map(|capacity| (capacity as f32).clamp(0.0, 100.0));
    let charging = read_trimmed(&supply.join("status")).is_some_and(|status| status == "Charging");
    (percent, charging)
}

/// Screen brightness as a 0.0..1.0 fraction from the first backlight under
/// `/sys/class/backlight`.
///
/// The Control Centre only displays the current value, so no write path is
/// provided; adjusting brightness stays with the session's own tools.
fn brightness() -> Option<f32> {
    let backlight = first_entry("/sys/class/backlight", "")?;
    let current = read_trimmed(&backlight.join("brightness"))?.parse::<f32>().ok()?;
    let max = read_trimmed(&backlight.join("max_brightness"))?
        .parse::<f32>()
        .ok()?;
    if max <= 0.0 {
        return None;
    }
    Some((current / max).clamp(0.0, 1.0))
}

fn cached_reading<T: Copy>(
    cache: &'static OnceLock<Mutex<CachedReading<T>>>,
    reader: fn() -> T,
    initial: T,
) -> T {
    let cache = cache.get_or_init(|| {
        Mutex::new(CachedReading {
            value: initial,
            refreshed_at: None,
        })
    });
    let mut cache = match cache.lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    if cache
        .refreshed_at
        .is_some_and(|refreshed_at| refreshed_at.elapsed() < COMMAND_READING_TTL)
    {
        return cache.value;
    }

    let value = reader();
    cache.value = value;
    cache.refreshed_at = Some(Instant::now());
    value
}

/// Default sink volume as a 0.0..1.0 fraction via `pactl`, the PulseAudio /
/// PipeWire control surface. A missing daemon or unusual output yields
/// `None`.
fn volume() -> Option<f32> {
    cached_reading(&VOLUME_READING, read_volume, None)
}

fn read_volume() -> Option<f32> {
    let output = match Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            warn!(?error, "pactl unavailable for the volume reading");
            return None;
        }
    };
    if !output.status.success() {
        warn!("pactl could not report the default sink volume");
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The line looks like `Volume: front-left: 45875 /  70% / -9.94 dB, ...`;
    // the percentage is the last token before the first `%`.
    stdout
        .lines()
        .find(|line| line.trim_start().starts_with("Volume:"))
        .and_then(|line| line.split('%').next())
        .and_then(|head| head.split_whitespace().next_back())
        .and_then(|percent| percent.parse::<f32>().ok())
        .map(|percent| (percent / 100.0).clamp(0.0, 1.0))
}

/// The connected interface's name and liveness. Wireless interfaces are
/// preferred through `/proc/net/wireless`; any non-loopback interface whose
/// sysfs `operstate` reads `up` counts as a fallback.
fn network() -> (Option<String>, bool) {
    let wireless = fs::read_to_string("/proc/net/wireless").unwrap_or_default();
    let wifi = wireless
        .lines()
        .skip(2)
        .find_map(|line| line.split(':').next().map(str::trim))
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    if let Some(name) = wifi {
        return (Some(name), true);
    }

    let Ok(entries) = fs::read_dir("/sys/class/net") else {
        return (None, false);
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "lo")
        .find(|name| {
            fs::read_to_string(format!("/sys/class/net/{name}/operstate"))
                .is_ok_and(|state| state.trim() == "up")
        })
        .map(|name| (Some(name), true))
        .unwrap_or((None, false))
}

/// CPU load percent from successive `/proc/stat` reads. A first sample, a
/// missing or malformed file, or a zero/reset delta reads as 0.0. This keeps
/// the read path non-blocking; `Rouch::refresh_readings` already spaces calls
/// out, and the next refresh supplies the second sample.
fn cpu_percent() -> f32 {
    let Some(current) = cpu_totals() else {
        return 0.0;
    };

    let previous = PREVIOUS_CPU_TOTALS.get_or_init(|| Mutex::new(None));
    let mut previous = match previous.lock() {
        Ok(previous) => previous,
        Err(poisoned) => poisoned.into_inner(),
    };
    previous
        .replace(current)
        .map(|old| cpu_percent_from_totals(old, current))
        .unwrap_or(0.0)
}

/// Convert two aggregate CPU samples into a bounded percentage.
fn cpu_percent_from_totals(previous: (f32, f32), current: (f32, f32)) -> f32 {
    let (idle_1, total_1) = previous;
    let (idle_2, total_2) = current;
    if !idle_1.is_finite() || !total_1.is_finite() || !idle_2.is_finite() || !total_2.is_finite() {
        return 0.0;
    }
    let total_delta = total_2 - total_1;
    if total_delta <= 0.0 {
        return 0.0;
    }
    let idle_delta = (idle_2 - idle_1).clamp(0.0, total_delta);
    ((total_delta - idle_delta) / total_delta * 100.0).clamp(0.0, 100.0)
}

/// Idle and total jiffies from `/proc/stat`'s aggregate `cpu` line.
fn cpu_totals() -> Option<(f32, f32)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let mut fields = stat.lines().next()?.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }

    let mut count = 0usize;
    let mut idle = 0.0;
    let mut total = 0.0;
    for (index, field) in fields.enumerate() {
        let value = field.parse::<f32>().ok()?;
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        if index == 3 || index == 4 {
            idle += value;
        }
        total += value;
        count += 1;
    }
    (count >= 4 && idle.is_finite() && total.is_finite()).then_some((idle, total))
}

/// RAM usage percent from `/proc/meminfo`'s `MemTotal` and `MemAvailable`.
fn memory_percent() -> f32 {
    let Ok(info) = fs::read_to_string("/proc/meminfo") else {
        return 0.0;
    };
    let total = meminfo_field(&info, "MemTotal");
    let available = meminfo_field(&info, "MemAvailable");
    if total <= 0.0 {
        return 0.0;
    }
    ((total - available) / total * 100.0).clamp(0.0, 100.0)
}

/// One `Field: kB` value from `/proc/meminfo`, in kibibytes.
fn meminfo_field(info: &str, field: &str) -> f32 {
    info.lines()
        .find_map(|line| {
            let rest = line.strip_prefix(field)?;
            rest.strip_prefix(':')
        })
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(0.0)
}

/// Root filesystem usage percent from `df -P /`. Errors degrade to 0.0.
fn storage_percent() -> f32 {
    cached_reading(&STORAGE_READING, read_storage_percent, 0.0)
}

fn read_storage_percent() -> f32 {
    let output = match Command::new("df").args(["-P", "/"]).output() {
        Ok(output) => output,
        Err(error) => {
            warn!(?error, "df unavailable for the storage reading");
            return 0.0;
        }
    };
    if !output.status.success() {
        warn!("df could not report the root filesystem usage");
        return 0.0;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(capacity) = stdout
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(4))
    else {
        return 0.0;
    };
    capacity
        .strip_suffix('%')
        .and_then(|percent| percent.parse::<f32>().ok())
        .unwrap_or(0.0)
        .clamp(0.0, 100.0)
}

/// The kernel hostname from `/etc/hostname`, or `"Rouch"` when unreadable.
fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Rouch".to_owned())
}

/// Read a sysfs-style file and trim its trailing newline.
fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|raw| raw.trim().to_owned())
}

/// The first directory under `parent` whose name starts with `prefix`, sorted
/// for determinism. An empty prefix accepts any directory. `/sys/class`
/// entries are symlinks, so directory-ness is resolved through the path.
fn first_entry(parent: &str, prefix: &str) -> Option<std::path::PathBuf> {
    let entries = fs::read_dir(parent).ok()?;
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(prefix))
        .collect();
    names.sort();
    names.first().map(|name| Path::new(parent).join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_math_is_bounded_and_non_blocking() {
        assert_eq!(cpu_percent_from_totals((10.0, 100.0), (10.0, 100.0)), 0.0);
        assert_eq!(cpu_percent_from_totals((10.0, 100.0), (30.0, 90.0)), 0.0);
        assert_eq!(cpu_percent_from_totals((10.0, 100.0), (10.0, 200.0)), 100.0);
        assert_eq!(cpu_percent_from_totals((10.0, 100.0), (60.0, 200.0)), 50.0);
    }

    #[test]
    fn malformed_cpu_totals_are_safe() {
        assert_eq!(cpu_percent_from_totals((f32::NAN, 100.0), (10.0, 200.0)), 0.0);
        assert_eq!(cpu_percent_from_totals((10.0, f32::NAN), (10.0, 200.0)), 0.0);
    }
}
