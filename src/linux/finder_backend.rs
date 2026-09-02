//! The Finder's real file backend: plain `std::fs` listings mapped onto
//! the pure Finder model in [`crate::finder`].
//!
//! Listing skips dot-files unless the user asked for them, folders open by
//! navigating (the caller receives the directory back) and files go to
//! `xdg-open`, exactly like the macOS rule. Nothing panics: an unreadable
//! directory simply lists empty.

// The Finder renderer samples this module; it ships compiled ahead of its
// caller.
#![allow(dead_code)]

use std::{cmp::Ordering, fs, path::Path, process::Command};

use crate::finder::{FinderItem, FinderSort};

/// Lists a real directory as Finder items.
pub fn list_dir(path: &str, show_hidden: bool) -> Vec<FinderItem> {
    let mut items = Vec::new();
    let Ok(entries) = fs::read_dir(path) else {
        return items;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(metadata) = fs::metadata(entry.path()) else {
            continue;
        };
        items.push(FinderItem {
            name,
            directory: metadata.is_dir(),
            size_bytes: metadata.len(),
        });
    }

    sort_items(&mut items, FinderSort::Name);
    items
}

/// The metadata of one path: name, directory flag and size.
pub fn stat_item(path: &Path) -> Option<FinderItem> {
    let metadata = fs::metadata(path).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(FinderItem {
        name,
        directory: metadata.is_dir(),
        size_bytes: metadata.len(),
    })
}

/// Opens an item the macOS way: folders navigate (the directory comes back),
/// files open through `xdg-open`.
pub fn open_path(path: &Path) -> Option<String> {
    if path.is_dir() {
        return Some(path.to_string_lossy().into_owned());
    }
    let _ = Command::new("xdg-open").arg(path).spawn();
    None
}

/// Sorts items into the chosen Finder order.
pub fn sort_items(items: &mut [FinderItem], sort: FinderSort) {
    match sort {
        FinderSort::Size => items.sort_by(|a, b| match (a.directory, b.directory) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => b.size_bytes.cmp(&a.size_bytes),
        }),
        // Date needs an extra metadata pass for mtimes; until then it keeps
        // the alphabetical order.
        FinderSort::Date | FinderSort::Name => {
            items.sort_by_key(|item| item.name.to_lowercase());
        }
    }
}

/// The Finder's starting path: the user's Desktop directory, with `/tmp`
/// and then `/` as fallbacks.
pub fn default_root() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let desktop = Path::new(&home).join("Desktop");
        if desktop.is_dir() {
            return desktop.to_string_lossy().into_owned();
        }
    }
    if Path::new("/tmp").is_dir() {
        return "/tmp".to_owned();
    }
    "/".to_owned()
}

/// Readable byte sizes: "1.2 KB", "3.4 MB", with any trailing ".0" trimmed.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];

    if bytes < 1024 {
        return format!("{bytes} bytes");
    }

    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    let rounded = format!("{value:.1}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed} {}", UNITS[unit])
}
