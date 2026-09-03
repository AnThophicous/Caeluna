//! Linux-side evidence collection for Game Mode.
//!
//! This adapter only reads bounded `/proc` files. It never changes process
//! priority, sends signals, edits cgroups, or assumes that a process name is a
//! game. The pure scorer in `crate::game_mode` remains the only place that
//! decides whether evidence is enough to act.

use std::{fs, io::Read, path::PathBuf};

use crate::game_mode::{
    CgroupSnapshot, GameSnapshot, MAX_CGROUP_SNAPSHOTS, MAX_PROCESS_SNAPSHOTS, PerformanceTier,
    ProcessSnapshot, WindowSnapshot,
};

const MAX_PROC_FILE_BYTES: usize = 16 * 1024;
const MAX_EXECUTABLE_BYTES: usize = 512;
const MAX_RUNTIME_ARGS: usize = 16;
const MAX_RUNTIME_ARG_BYTES: usize = 256;

/// Bounded process/cgroup inventory collected at one event-loop sample.
#[derive(Debug, Default)]
pub struct ProcessInventory {
    pub processes: Vec<ProcessSnapshot>,
    pub cgroups: Vec<CgroupSnapshot>,
}

/// Collect only the process inventory needed by Game Mode.
///
/// The focused PID is read first so a busy `/proc` directory cannot omit the
/// foreground client merely because the directory iterator reaches its cap.
pub fn collect_process_inventory(foreground_pid: Option<u32>) -> ProcessInventory {
    let mut inventory = ProcessInventory {
        processes: Vec::with_capacity(64),
        cgroups: Vec::with_capacity(64),
    };

    if let Some(pid) = foreground_pid {
        collect_one(pid, &mut inventory);
    }

    let Ok(entries) = fs::read_dir("/proc") else {
        return inventory;
    };

    for entry in entries.flatten() {
        if inventory.processes.len() >= MAX_PROCESS_SNAPSHOTS {
            break;
        }
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        if Some(pid) == foreground_pid {
            continue;
        }
        collect_one(pid, &mut inventory);
    }

    inventory
}

/// Build the complete detector input from compositor-known windows and the
/// current foreground PID. Keeping this function small makes it useful to the
/// native and nested Linux paths alike.
pub fn collect_snapshot(
    windows: Vec<WindowSnapshot>,
    desktop_entries: Vec<crate::game_mode::DesktopEntrySnapshot>,
    foreground_pid: Option<u32>,
) -> GameSnapshot {
    let inventory = collect_process_inventory(foreground_pid);
    GameSnapshot {
        processes: inventory.processes,
        windows,
        desktop_entries,
        cgroups: inventory.cgroups,
        foreground_pid,
        manual_confirmation: false,
    }
}

/// Select the shell policy tier from `/proc/meminfo` and logical CPU count.
/// Missing host facts deliberately fall back to Balanced rather than guessing
/// from a product name or applying the most aggressive policy.
pub fn hardware_tier() -> PerformanceTier {
    let memory_bytes = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|body| parse_mem_total_bytes(&body))
        .unwrap_or(0);
    let logical_cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    if memory_bytes == 0 {
        PerformanceTier::Balanced
    } else {
        PerformanceTier::from_hardware(memory_bytes, logical_cpus)
    }
}

fn collect_one(pid: u32, inventory: &mut ProcessInventory) {
    if inventory.processes.len() >= MAX_PROCESS_SNAPSHOTS {
        return;
    }
    let process = read_process(pid);
    if let Some(process) = process {
        inventory.processes.push(process);
    }
    if inventory.cgroups.len() < MAX_CGROUP_SNAPSHOTS {
        if let Some(cgroup) = read_cgroup(pid) {
            inventory.cgroups.push(cgroup);
        }
    }
}

fn read_process(pid: u32) -> Option<ProcessSnapshot> {
    let root = proc_path(pid, "");
    let parent_pid = read_parent_pid(&root);
    let executable = read_executable(&root)?;
    let argv = read_runtime_args(&root);
    let environment_keys = read_environment_keys(&root);

    Some(ProcessSnapshot {
        pid,
        parent_pid,
        executable,
        argv,
        environment_keys,
        desktop_id: None,
    })
}

fn read_executable(root: &PathBuf) -> Option<String> {
    let link = fs::read_link(root.join("exe"))
        .ok()
        .map(|path| truncate_string(path.to_string_lossy().as_ref(), MAX_EXECUTABLE_BYTES));
    link.or_else(|| {
        read_limited(&root.join("comm"), 128)
            .map(|bytes| truncate_string(String::from_utf8_lossy(&bytes).trim(), MAX_EXECUTABLE_BYTES))
    })
}

fn read_parent_pid(root: &PathBuf) -> Option<u32> {
    let bytes = read_limited(&root.join("stat"), 4096)?;
    let text = String::from_utf8_lossy(&bytes);
    let after_name = text.rsplit_once(')')?.1;
    after_name
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u32>().ok())
}

/// Keep only marker-bearing argv tokens. Full command lines can contain
/// secrets and are not useful to the scorer, so they must not enter the
/// compositor's diagnostic state.
fn read_runtime_args(root: &PathBuf) -> Vec<String> {
    let Some(bytes) = read_limited(&root.join("cmdline"), MAX_PROC_FILE_BYTES) else {
        return Vec::new();
    };
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|token| std::str::from_utf8(token).ok())
        .filter(|token| runtime_argument_hint(token))
        .take(MAX_RUNTIME_ARGS)
        .map(|token| truncate_string(token, MAX_RUNTIME_ARG_BYTES))
        .collect()
}

/// Preserve selected environment *keys* only. Values can contain credentials,
/// paths, or user data and are intentionally thrown away at the read boundary.
fn read_environment_keys(root: &PathBuf) -> Vec<String> {
    let Some(bytes) = read_limited(&root.join("environ"), MAX_PROC_FILE_BYTES) else {
        return Vec::new();
    };
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .filter_map(|entry| entry.split_once('=').map(|(key, _)| key).or(Some(entry)))
        .map(str::trim)
        .filter(|key| relevant_environment_key(key))
        .take(32)
        .map(ToOwned::to_owned)
        .collect()
}

fn read_cgroup(pid: u32) -> Option<CgroupSnapshot> {
    let root = proc_path(pid, "");
    let bytes = read_limited(&root.join("cgroup"), 4096)?;
    let mut paths = Vec::new();
    let mut controllers = Vec::new();
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines().take(16) {
        let Some((controller_text, path)) = line.split_once("::") else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        paths.push(path);
        controllers.extend(
            controller_text
                .split(',')
                .filter(|name| !name.is_empty())
                .take(8)
                .map(ToOwned::to_owned),
        );
    }
    let path = paths.into_iter().next()?;
    Some(CgroupSnapshot {
        pid: Some(pid),
        path: truncate_string(path, 512),
        controllers,
    })
}

fn proc_path(pid: u32, suffix: &str) -> PathBuf {
    let mut path = PathBuf::from("/proc");
    path.push(pid.to_string());
    if !suffix.is_empty() {
        path.push(suffix);
    }
    path
}

fn read_limited(path: &PathBuf, max_bytes: usize) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(max_bytes.min(4096));
    file.take(max_bytes as u64).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn truncate_string(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}

fn runtime_argument_hint(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '/' | '\\' | ':' | '=' | '.')
        })
        .any(|token| {
            token.starts_with("steam_app_")
                || token == "proton"
                || token.starts_with("proton-")
                || token.starts_with("proton_")
                || token == "wine"
                || token.starts_with("wine-")
                || token.starts_with("wine_")
                || token == "sober"
                || token.starts_with("sober-")
                || token.starts_with("sober_")
                || token == "pressure-vessel"
                || token.starts_with("pressure-vessel-")
                || token.starts_with("pressure-vessel_")
                || token == "pressure_vessel"
                || token.starts_with("pressure_vessel-")
                || token.starts_with("pressure_vessel_")
        })
}

fn relevant_environment_key(key: &str) -> bool {
    let upper = key.trim().to_ascii_uppercase();
    upper == "STEAM_COMPAT_DATA_PATH"
        || upper == "STEAM_APPID"
        || upper == "STEAM_GAME_ID"
        || upper == "WINEPREFIX"
        || upper == "WINEARCH"
        || upper == "WINELOADER"
        || upper.starts_with("WINE_")
        || upper.starts_with("PROTON_")
        || upper.starts_with("PRESSURE_VESSEL_")
        || upper == "SOBER"
        || upper.starts_with("SOBER_")
}

fn parse_mem_total_bytes(body: &str) -> Option<u64> {
    body.lines()
        .find(|line| line.split_whitespace().next() == Some("MemTotal:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_argument_filter_ignores_arbitrary_game_like_names() {
        assert!(runtime_argument_hint("/usr/lib/proton").then_some(()).is_some());
        assert!(runtime_argument_hint("steam_app_1234").then_some(()).is_some());
        assert!(!runtime_argument_hint("my-game-launcher"));
    }

    #[test]
    fn environment_filter_keeps_keys_but_not_values() {
        assert!(relevant_environment_key("STEAM_COMPAT_DATA_PATH"));
        assert!(relevant_environment_key("PROTON_LOG"));
        assert!(!relevant_environment_key("TOKEN"));
    }

    #[test]
    fn truncate_preserves_utf8_boundaries() {
        assert_eq!(truncate_string("ábc", 1), "");
        assert_eq!(truncate_string("ábc", 2), "á");
    }

    #[test]
    fn meminfo_parser_requires_memtotal_and_converts_kib() {
        assert_eq!(
            parse_mem_total_bytes("MemFree: 2 kB\nMemTotal: 4096 kB\n"),
            Some(4 * 1024 * 1024)
        );
        assert_eq!(parse_mem_total_bytes("MemFree: 2 kB\n"), None);
    }
}
