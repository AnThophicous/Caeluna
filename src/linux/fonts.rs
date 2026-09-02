//! The SF Pro typography system: real TTF/OTF text when a font is installed.
//!
//! Apple's San Francisco licence forbids bundling the TTFs, so Rouch ships
//! without them. At startup `FontBook::load` searches the user's and the
//! system's font directories for an installed SF Pro; when none is present
//! it falls back metrically to Inter, Ubuntu or DejaVu Sans. When no TTF at
//! all is found the fields stay `None` and every caller silently keeps
//! drawing with the `pixel` micro-font.

// The shell, dock and welcome renderers sample this module from their next
// milestone; it ships compiled ahead of its callers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use fontdue::{Font, FontSettings};
use tracing::warn;

use crate::linux::pixel;
use crate::windowing::Rect;

/// Filename patterns for the SF Pro family, in preference order.
const SF_PRO: [&str; 3] = ["sf-pro", "sfpro", "sanfrancisco"];
/// Metric-compatible fallbacks, in preference order.
const FALLBACKS: [&str; 4] = ["inter", "ubuntu", "dejavusans", "dejavu"];

/// The loaded faces of one typographic family.
pub struct FontBook {
    pub regular: Option<Font>,
    pub bold: Option<Font>,
    pub family_name: String,
}

impl FontBook {
    /// Procura SF Pro nos dirs do usuário e do sistema; fallback Inter/Ubuntu/DejaVu.
    /// Nunca entra em pânico; sem fontes, campos None e callers caem na micro-fonte.
    pub fn load() -> Self {
        let files = font_directories()
            .iter()
            .flat_map(|dir| scan_dir(dir, 0))
            .collect::<Vec<PathBuf>>();

        // SF Pro first, then the metric fallbacks, pattern by pattern; the
        // bold face is always searched with the same pattern as the regular.
        for pattern in SF_PRO.iter().chain(FALLBACKS.iter()) {
            if let Some((font, path)) = find_regular(&files, pattern) {
                let family_name = font.name().map(str::to_owned).unwrap_or_else(|| file_stem(&path));
                let bold = find_bold(&files, pattern).or_else(|| load_font(&path));
                return Self {
                    regular: Some(font),
                    bold,
                    family_name,
                };
            }
        }

        Self {
            regular: None,
            bold: None,
            family_name: "Rouch Micro".into(),
        }
    }

    /// Desenha texto rasterizado TTF em buffer BGRA. Retorna a largura desenhada.
    /// Font None → cai em pixel::draw_text e retorna text_width da micro-fonte.
    pub fn draw_text(
        &self,
        pixels: &mut [u8],
        width: usize,
        rect: Rect,
        color: [u8; 4],
        text: &str,
        px_height: f32,
    ) -> i32 {
        let Some(font) = self.regular.as_ref() else {
            pixel::draw_text(pixels, width, rect, color, text);
            return pixel::text_width(text);
        };

        let buffer_height = pixels.len() / (width * 4).max(1);
        let ascent = font
            .horizontal_line_metrics(px_height)
            .map(|line| line.ascent)
            .unwrap_or(px_height * 0.8);
        let baseline = rect.origin.y.max(0) + ascent.round() as i32;
        let clip = clip_rect(rect, width, buffer_height);

        let start = clip.origin.x as f32;
        let mut pen_x = start;
        for ch in text.chars() {
            let index = font.lookup_glyph_index(ch);
            let (metrics, bitmap) = font.rasterize_indexed(index, px_height);
            let glyph = GlyphRaster {
                metrics: &metrics,
                bitmap: &bitmap,
            };
            blit_glyph(pixels, width, &glyph, pen_x, baseline, color, clip);
            pen_x += metrics.advance_width;
        }
        (pen_x - start) as i32
    }

    /// Mede a largura do texto no px_height dado.
    pub fn text_width(&self, text: &str, px_height: f32) -> i32 {
        let Some(font) = self.regular.as_ref() else {
            return pixel::text_width(text);
        };
        text.chars()
            .map(|ch| font.metrics(ch, px_height).advance_width)
            .sum::<f32>() as i32
    }
}

/// The directories scanned for SF Pro, in look-up order.
fn font_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".fonts"));
        dirs.push(home.join(".local").join("share").join("fonts"));
    }
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs
}

/// List files under `dir`, descending at most two directory levels.
fn scan_dir(dir: &Path, depth: u8) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if depth < 2 {
                files.extend(scan_dir(&path, depth + 1));
            }
        } else if kind.is_file() {
            files.push(path);
        }
    }
    files
}

/// The first non-bold face matching `pattern`.
fn find_regular(files: &[PathBuf], pattern: &str) -> Option<(Font, PathBuf)> {
    for file in files {
        if !matches_face(file, pattern) || has_bold(file) {
            continue;
        }
        if let Some(font) = load_font(file) {
            return Some((font, file.clone()));
        }
    }
    None
}

/// The first bold variant matching `pattern`.
fn find_bold(files: &[PathBuf], pattern: &str) -> Option<Font> {
    for file in files {
        if !matches_face(file, pattern) || !has_bold(file) {
            continue;
        }
        if let Some(font) = load_font(file) {
            return Some(font);
        }
    }
    None
}

/// Case-insensitive filename match on a `.ttf`/`.otf` file.
fn matches_face(file: &Path, pattern: &str) -> bool {
    let Some(name) = file.file_name().and_then(std::ffi::OsStr::to_str) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    (lower.ends_with(".ttf") || lower.ends_with(".otf")) && lower.contains(pattern)
}

/// Whether the filename marks a bold weight.
fn has_bold(file: &Path) -> bool {
    file.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.to_ascii_lowercase().contains("bold"))
}

/// Parse one font file, warning instead of panicking on any failure.
fn load_font(file: &Path) -> Option<Font> {
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(path = ?file, ?error, "Font file could not be read");
            return None;
        }
    };
    match Font::from_bytes(bytes, FontSettings::default()) {
        Ok(font) => Some(font),
        Err(error) => {
            warn!(path = ?file, error, "Font file could not be parsed");
            None
        }
    }
}

/// The filename without its extension, as a family-name fallback.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("Rouch")
        .to_owned()
}

/// Intersect a text box with the buffer bounds, the blit's clip rectangle.
fn clip_rect(rect: Rect, width: usize, buffer_height: usize) -> Rect {
    Rect::new(
        rect.origin.x.max(0),
        rect.origin.y.max(0),
        rect.right().min(width as i32) - rect.origin.x.max(0),
        rect.bottom().min(buffer_height as i32) - rect.origin.y.max(0),
    )
}

/// One rasterized glyph, metrics and coverage vector together.
struct GlyphRaster<'a> {
    metrics: &'a fontdue::Metrics,
    bitmap: &'a [u8],
}

/// Blit one rasterized glyph onto the BGRA buffer, clipped to the text box
/// and the buffer bounds; the coverage vector becomes blend alpha.
fn blit_glyph(
    pixels: &mut [u8],
    width: usize,
    glyph: &GlyphRaster,
    pen_x: f32,
    baseline: i32,
    color: [u8; 4],
    clip: Rect,
) {
    let metrics = glyph.metrics;
    if metrics.width == 0 || glyph.bitmap.is_empty() {
        return;
    }

    // fontdue bitmaps start at the glyph's top-left corner; the math-space
    // top is `ymin + height`, which is `height` rows above the baseline on
    // the screen's down-facing y axis.
    let left = pen_x as i32 + metrics.xmin;
    let top = baseline - (metrics.ymin + metrics.height as i32);

    for (row, line) in glyph.bitmap.chunks_exact(metrics.width).enumerate() {
        let y = top + row as i32;
        if y < clip.origin.y || y >= clip.bottom() {
            continue;
        }
        for (col, coverage) in line.iter().enumerate() {
            if *coverage == 0 {
                continue;
            }
            let x = left + col as i32;
            if x < clip.origin.x || x >= clip.right() {
                continue;
            }
            let offset = (y as usize * width + x as usize) * 4;
            pixel::blend(pixels, offset, color, f32::from(*coverage) / 255.0);
        }
    }
}
