//! Software pixel helpers shared by every Rouch-drawn surface.
//!
//! Everything the chrome, dock and welcome screens paint goes through these
//! primitives over BGRA (`Fourcc::Argb8888`) memory: exact fills, rounded
//! rectangles, circles, and the 5x7 micro-font. Blending is pre-multiplied
//! source-over, matching what the GLES renderer expects.

use crate::windowing::Rect;

const BYTES_PER_PIXEL: usize = 4;

/// Return the usable dimensions of a tightly packed BGRA buffer.
///
/// Renderers normally hand us exactly `width * height * 4` bytes, but these
/// helpers are also used by fallback surfaces. Treat a zero width, an
/// overflowing stride, and a truncated last row as an empty drawing target;
/// drawing code must never turn malformed client data into a compositor
/// panic.
#[inline]
fn buffer_dimensions(pixels: &[u8], width: usize) -> Option<(usize, usize)> {
    let row_bytes = width.checked_mul(BYTES_PER_PIXEL)?;
    if row_bytes == 0 {
        return None;
    }
    Some((width, pixels.len() / row_bytes))
}

/// Clip a logical rectangle to the actual rows available in `pixels`.
///
/// The arithmetic intentionally happens in `i64`: [`Rect::right`] and
/// [`Rect::bottom`] use `i32` addition and are not suitable for hostile or
/// partially initialised geometry at a renderer boundary.
#[inline]
fn clipped_rect(pixels: &[u8], width: usize, rect: Rect) -> Option<(usize, usize, usize, usize)> {
    let (buffer_width, buffer_height) = buffer_dimensions(pixels, width)?;
    let max_x = buffer_width as i64;
    let max_y = buffer_height as i64;
    let rect_right = i64::from(rect.origin.x) + i64::from(rect.size.width);
    let rect_bottom = i64::from(rect.origin.y) + i64::from(rect.size.height);

    let x0 = i64::from(rect.origin.x).clamp(0, max_x) as usize;
    let y0 = i64::from(rect.origin.y).clamp(0, max_y) as usize;
    let x1 = rect_right.clamp(0, max_x) as usize;
    let y1 = rect_bottom.clamp(0, max_y) as usize;

    (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
}

#[inline]
fn fill_row(pixels: &mut [u8], width: usize, y: usize, x0: usize, x1: usize, color: [u8; 4]) {
    let Some(row_bytes) = width.checked_mul(BYTES_PER_PIXEL) else {
        return;
    };
    let Some(row_start) = y.checked_mul(row_bytes) else {
        return;
    };
    let Some(row) = pixels.get_mut(row_start..row_start.saturating_add(row_bytes)) else {
        return;
    };
    let Some(start) = x0.checked_mul(BYTES_PER_PIXEL) else {
        return;
    };
    let Some(end) = x1.checked_mul(BYTES_PER_PIXEL) else {
        return;
    };
    let Some(row) = row.get_mut(start..end) else {
        return;
    };
    for px in row.chunks_exact_mut(BYTES_PER_PIXEL) {
        px.copy_from_slice(&color);
    }
}

/// Draw `color` (BGRA) into a rectangle of a `width`-pixel-wide BGRA buffer.
pub fn fill_rect(pixels: &mut [u8], width: usize, rect: Rect, color: [u8; 4]) {
    let Some((x0, y0, x1, y1)) = clipped_rect(pixels, width, rect) else {
        return;
    };

    for y in y0..y1 {
        fill_row(pixels, width, y, x0, x1, color);
    }
}

/// Draw a rounded rectangle by skipping the corner cut-outs, giving the soft
/// geometry Material You asks for. Radius is clamped to half the short side.
pub fn fill_round_rect(pixels: &mut [u8], width: usize, rect: Rect, radius: i32, color: [u8; 4]) {
    if rect.size.width <= 0 || rect.size.height <= 0 {
        return;
    }
    let max_radius = (rect.size.width.min(rect.size.height) / 2).max(0);
    let radius = radius.max(0).min(max_radius);

    if radius == 0 {
        fill_rect(pixels, width, rect, color);
        return;
    }

    let Some((x0, y0, x1, y1)) = clipped_rect(pixels, width, rect) else {
        return;
    };
    let radius = radius as i64;
    let radius_sq = (radius as f32) * (radius as f32);
    let rect_right = i64::from(rect.origin.x) + i64::from(rect.size.width);
    let rect_bottom = i64::from(rect.origin.y) + i64::from(rect.size.height);

    for y in y0..y1 {
        // Distance from the rounded band's natural edge, in pixels.
        let dy = {
            let top = y as i64 - i64::from(rect.origin.y);
            let bottom = rect_bottom - 1 - y as i64;
            let edge = top.min(bottom);
            if edge >= radius { 0 } else { radius - edge }
        };
        let dy_sq = (dy as f32) * (dy as f32);

        // Most rows of a rounded rectangle are rectangular. Avoid walking
        // every pixel and recomputing the horizontal edge for those rows.
        if dy == 0 {
            fill_row(pixels, width, y, x0, x1, color);
            continue;
        }

        for x in x0..x1 {
            let dx = {
                let left = x as i64 - i64::from(rect.origin.x);
                let right = rect_right - 1 - x as i64;
                let edge = left.min(right);
                if edge >= radius { 0 } else { radius - edge }
            };
            let dx_sq = (dx as f32) * (dx as f32);
            if dx_sq + dy_sq <= radius_sq {
                let Some(offset) = y
                    .checked_mul(width)
                    .and_then(|row| row.checked_add(x))
                    .and_then(|pixel| pixel.checked_mul(BYTES_PER_PIXEL))
                else {
                    continue;
                };
                if let Some(px) = pixels.get_mut(offset..offset.saturating_add(BYTES_PER_PIXEL)) {
                    px.copy_from_slice(&color);
                }
            }
        }
    }
}

/// Fill a circle of `radius` around (`cx`, `cy`), with a 1px anti-aliased
/// rim that the traffic lights and indicator dots rely on.
pub fn fill_circle(pixels: &mut [u8], width: usize, cx: i32, cy: i32, radius: i32, color: [u8; 4]) {
    let Some((buffer_width, buffer_height)) = buffer_dimensions(pixels, width) else {
        return;
    };
    if radius < 0 {
        return;
    }

    let radius = i64::from(radius);
    let min_x = (i64::from(cx) - radius - 1).clamp(0, buffer_width as i64) as usize;
    let max_x = (i64::from(cx) + radius + 1).clamp(0, buffer_width as i64) as usize;
    let min_y = (i64::from(cy) - radius - 1).clamp(0, buffer_height as i64) as usize;
    let max_y = (i64::from(cy) + radius + 1).clamp(0, buffer_height as i64) as usize;
    let r = radius as f32;
    let cxf = cx as f32;
    let cyf = cy as f32;

    for y in min_y..max_y {
        for x in min_x..max_x {
            let dx = x as f32 + 0.5 - cxf;
            let dy = y as f32 + 0.5 - cyf;
            let dist = (dx * dx + dy * dy).sqrt();
            let coverage = ((r + 0.5) - dist).clamp(0.0, 1.0);
            if coverage > 0.0 {
                let offset = (y * width + x) * 4;
                blend(pixels, offset, color, coverage);
            }
        }
    }
}

/// Source-over blend of `color` (BGRA, straight alpha in the last channel)
/// into `pixels` at `offset`, with a fractional `coverage`.
#[inline]
pub fn blend(pixels: &mut [u8], offset: usize, color: [u8; 4], coverage: f32) {
    let Some(end) = offset.checked_add(BYTES_PER_PIXEL) else {
        return;
    };
    let Some(pixel) = pixels.get_mut(offset..end) else {
        return;
    };
    if !coverage.is_finite() {
        return;
    }
    let coverage = coverage.clamp(0.0, 1.0);
    let alpha = (color[3] as f32 * coverage) as u32;
    let inv = 255 - alpha;

    for i in 0..4 {
        let src = color[i] as u32 * alpha;
        let base = pixel[i] as u32 * inv;
        pixel[i] = ((src + base + 127) / 255) as u8;
    }
}

/// Draw one line of micro-font text inside a `width`-pixel-wide buffer.
///
/// `rect` bounds the text box; the glyphs are laid out from `rect`'s origin
/// with `line_height` spacing. Only ASCII renders; other bytes become a
/// blank column so UTF-8 never shows mojibake.
pub fn draw_text(pixels: &mut [u8], width: usize, rect: Rect, color: [u8; 4], text: &str) {
    let Some((buffer_width, buffer_height)) = buffer_dimensions(pixels, width) else {
        return;
    };
    let glyph_width = 5usize;
    let advance = glyph_width + 1;
    let right =
        (i64::from(rect.origin.x) + i64::from(rect.size.width)).clamp(0, buffer_width as i64) as usize;
    let left = i64::from(rect.origin.x).clamp(0, buffer_width as i64) as usize;
    if left >= right {
        return;
    }

    for (i, character) in text.chars().enumerate() {
        let Some(x0) = i.checked_mul(advance).and_then(|offset| left.checked_add(offset)) else {
            break;
        };
        if x0.checked_add(glyph_width).is_none_or(|end| end > right) {
            break;
        }
        let Some(rows) = glyph(character as u8) else {
            continue;
        };

        for (row, bits) in rows.iter().enumerate() {
            let y = i64::from(rect.origin.y) + row as i64;
            if y < 0 || y >= buffer_height as i64 {
                continue;
            }
            for col in 0..glyph_width {
                if bits & (1 << (glyph_width - 1 - col)) != 0 {
                    let x = x0 + col;
                    if x < buffer_width {
                        let Some(offset) = (y as usize)
                            .checked_mul(width)
                            .and_then(|row| row.checked_add(x))
                            .and_then(|pixel| pixel.checked_mul(BYTES_PER_PIXEL))
                        else {
                            continue;
                        };
                        blend(pixels, offset, color, 1.0);
                    }
                }
            }
        }
    }
}

/// Measure a micro-font string in pixels, for centring labels.
pub fn text_width(text: &str) -> i32 {
    let glyphs = text.chars().count();
    if glyphs == 0 {
        return 0;
    }
    glyphs.saturating_mul(6).saturating_sub(1).min(i32::MAX as usize) as i32
}

// A 5x7 micro-font of printable ASCII. Each glyph is seven rows of five
// bits, most significant bit leftmost in every row.
const GLYPHS: [(u8, [u128; 7]); 73] = [
    (b'A', [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11]),
    (b'B', [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E]),
    (b'C', [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E]),
    (b'D', [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C]),
    (b'E', [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F]),
    (b'F', [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10]),
    (b'G', [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F]),
    (b'H', [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11]),
    (b'I', [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    (b'J', [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C]),
    (b'K', [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11]),
    (b'L', [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F]),
    (b'M', [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11]),
    (b'N', [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11]),
    (b'O', [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E]),
    (b'P', [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10]),
    (b'Q', [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D]),
    (b'R', [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11]),
    (b'S', [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E]),
    (b'T', [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04]),
    (b'U', [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E]),
    (b'V', [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04]),
    (b'W', [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A]),
    (b'X', [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11]),
    (b'Y', [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04]),
    (b'Z', [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F]),
    (b'a', [0x00, 0x00, 0x0E, 0x01, 0x0F, 0x11, 0x0F]),
    (b'b', [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x1E]),
    (b'c', [0x00, 0x00, 0x0F, 0x10, 0x10, 0x10, 0x0F]),
    (b'd', [0x01, 0x01, 0x0F, 0x11, 0x11, 0x11, 0x0F]),
    (b'e', [0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E]),
    (b'f', [0x06, 0x08, 0x1C, 0x08, 0x08, 0x08, 0x08]),
    (b'g', [0x00, 0x0F, 0x11, 0x11, 0x0F, 0x01, 0x0E]),
    (b'h', [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x11]),
    (b'i', [0x04, 0x00, 0x0C, 0x04, 0x04, 0x04, 0x0E]),
    (b'j', [0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0C]),
    (b'k', [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12]),
    (b'l', [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    (b'm', [0x00, 0x00, 0x1A, 0x15, 0x15, 0x15, 0x15]),
    (b'n', [0x00, 0x00, 0x1E, 0x11, 0x11, 0x11, 0x11]),
    (b'o', [0x00, 0x00, 0x0E, 0x11, 0x11, 0x11, 0x0E]),
    (b'p', [0x00, 0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10]),
    (b'q', [0x00, 0x0F, 0x11, 0x11, 0x0F, 0x01, 0x01]),
    (b'r', [0x00, 0x00, 0x16, 0x09, 0x08, 0x08, 0x08]),
    (b's', [0x00, 0x00, 0x0F, 0x10, 0x0E, 0x01, 0x1E]),
    (b't', [0x08, 0x08, 0x1C, 0x08, 0x08, 0x08, 0x06]),
    (b'u', [0x00, 0x00, 0x11, 0x11, 0x11, 0x11, 0x0F]),
    (b'v', [0x00, 0x00, 0x11, 0x11, 0x11, 0x0A, 0x04]),
    (b'w', [0x00, 0x00, 0x11, 0x15, 0x15, 0x15, 0x0A]),
    (b'x', [0x00, 0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11]),
    (b'y', [0x00, 0x11, 0x11, 0x11, 0x0F, 0x01, 0x0E]),
    (b'z', [0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F]),
    (b'0', [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E]),
    (b'1', [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    (b'2', [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F]),
    (b'3', [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E]),
    (b'4', [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02]),
    (b'5', [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E]),
    (b'6', [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E]),
    (b'7', [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08]),
    (b'8', [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E]),
    (b'9', [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C]),
    (b' ', [0, 0, 0, 0, 0, 0, 0]),
    (b'.', [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C]),
    (b',', [0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08]),
    (b'-', [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00]),
    (b':', [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00]),
    (b'/', [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10]),
    (b'_', [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F]),
    (b'(', [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02]),
    (b')', [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08]),
    (b'!', [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04]),
    (b'?', [0x0E, 0x11, 0x01, 0x06, 0x04, 0x00, 0x04]),
];

/// Look up the rows for one ASCII byte, or `None` for unhandled bytes.
fn glyph(byte: u8) -> Option<&'static [u128; 7]> {
    GLYPHS.iter().find(|(key, _)| *key == byte).map(|(_, rows)| rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_outside_the_buffer_is_a_noop() {
        let mut pixels = vec![0x5a; 4 * 4 * 4];
        let original = pixels.clone();

        fill_rect(&mut pixels, 4, Rect::new(20, 20, 4, 4), [1, 2, 3, 4]);
        fill_round_rect(&mut pixels, 4, Rect::new(-20, -20, 4, 4), 12, [1, 2, 3, 4]);
        fill_circle(&mut pixels, 4, i32::MAX, i32::MIN, i32::MAX, [1, 2, 3, 4]);

        assert_eq!(pixels, original);
    }

    #[test]
    fn malformed_stride_and_short_rows_never_panic() {
        let mut pixels = vec![0; 7];
        fill_rect(&mut pixels, 0, Rect::new(0, 0, 1, 1), [1, 2, 3, 4]);
        fill_rect(&mut pixels, usize::MAX, Rect::new(0, 0, 1, 1), [1, 2, 3, 4]);
        fill_round_rect(&mut pixels, 2, Rect::new(0, 0, 2, 2), 1, [1, 2, 3, 4]);
        draw_text(&mut pixels, 2, Rect::new(0, 0, 2, 7), [1, 2, 3, 4], "A");
        assert_eq!(pixels.len(), 7);
    }

    #[test]
    fn invalid_blend_offsets_and_coverage_are_safe() {
        let mut pixels = vec![0x5a; 4];
        blend(&mut pixels, usize::MAX, [1, 2, 3, 4], 1.0);
        blend(&mut pixels, 0, [1, 2, 3, 4], f32::NAN);
        assert_eq!(pixels, vec![0x5a; 4]);
    }

    #[test]
    fn text_measurement_is_empty_safe_and_counts_characters() {
        assert_eq!(text_width(""), 0);
        assert_eq!(text_width("ab"), 11);
        assert_eq!(text_width("é"), 5);
    }
}
