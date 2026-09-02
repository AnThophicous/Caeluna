//! The desktop wallpaper renderer.
//!
//! Decodes the supplied ocean `Wallpaper.webp` once and paints it into an
//! output-sized BGRA buffer using cover-fit cropping, so the image fills the
//! screen without distortion at any host window size.

use std::path::Path;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportAll, ImportMem, Renderer,
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
        },
    },
    utils::Point as PhysPoint,
};

use crate::windowing::Size;

/// Owns the decoded wallpaper and its render surface.
pub struct WallpaperRenderer {
    buffer: MemoryRenderBuffer,
    sized_for: (i32, i32),
    decoded: Vec<u8>,
    decoded_size: Size,
}

impl WallpaperRenderer {
    /// Decode `Wallpaper.webp` next to the executable or the project root.
    pub fn new(source: &Path) -> Option<Self> {
        let bytes = std::fs::read(source).ok()?;
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP)
            .ok()?
            .to_rgba8();
        let (w, h) = decoded.dimensions();
        let decoded = flip_to_bgra(decoded.into_raw(), w as usize);

        Some(Self {
            buffer: MemoryRenderBuffer::new(
                Fourcc::Argb8888,
                (8, 8),
                1,
                smithay::utils::Transform::Normal,
                None,
            ),
            sized_for: (0, 0),
            decoded,
            decoded_size: Size::new(w as i32, h as i32),
        })
    }

    /// Paint the wallpaper for the current output and return its element.
    pub fn render_element<R>(
        &mut self,
        renderer: &mut R,
        output_size: Size,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let width = output_size.width.max(1);
        let height = output_size.height.max(1);
        if self.sized_for != (width, height) {
            let mut context = self.buffer.render();
            context.resize((width, height));
            self.sized_for = (width, height);
        }

        {
            let mut context = self.buffer.render();
            let (w, h) = (width as usize, height as usize);
            let _ = context.draw(|pixels| {
                paint_cover(pixels, w, h, &self.decoded, self.decoded_size);
                Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                    smithay::utils::Size::from((width, height)),
                )])
            });
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            PhysPoint::from((0.0, 0.0)),
            &self.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

/// Paint a cover-fit crop of the decoded image into the output buffer.
fn paint_cover(pixels: &mut [u8], width: usize, height: usize, source: &[u8], source_size: Size) {
    if source.is_empty() || source_size.width <= 0 || source_size.height <= 0 {
        // No wallpaper: deep ocean fill so the desktop is never black.
        for px in pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[40, 20, 8, 255]);
        }
        return;
    }

    let crop = crate::dock::cover_fit(
        source_size,
        crate::windowing::Size::new(width as i32, height as i32),
    );
    let src_w = source_size.width as usize;

    let scale_x = crop.size.width as f32 / width as f32;
    let scale_y = crop.size.height as f32 / height as f32;

    for y in 0..height {
        let src_y =
            (crop.origin.y as usize + (y as f32 * scale_y) as usize).min(source_size.height as usize - 1);
        for x in 0..width {
            let src_x = (crop.origin.x as usize + (x as f32 * scale_x) as usize).min(src_w - 1);
            let src = (src_y * src_w + src_x) * 4;
            let dst = (y * width + x) * 4;
            pixels[dst..dst + 4].copy_from_slice(&source[src..src + 4]);
        }
    }
}

/// Convert RGBA rows into BGRA, the GLES `Argb8888` byte order, and flip
/// vertically: GL textures originate bottom-left.
fn flip_to_bgra(mut rgba: Vec<u8>, width: usize) -> Vec<u8> {
    let height = if width > 0 { rgba.len() / (width * 4) } else { 0 };
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }

    let mut flipped = rgba.clone();
    for row in 0..height {
        let src = row * width * 4;
        let dst = (height - 1 - row) * width * 4;
        flipped[dst..dst + width * 4].copy_from_slice(&rgba[src..src + width * 4]);
    }
    flipped
}

/// Candidate locations for the bundled wallpaper.
pub fn wallpaper_candidates() -> Vec<std::path::PathBuf> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_owned()));
    let mut roots: Vec<std::path::PathBuf> = exe.into_iter().collect();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }

    let mut candidates = roots
        .iter()
        .flat_map(|root| {
            [
                root.join("Wallpaper.webp"),
                root.join("..").join("Wallpaper.webp"),
                root.join("..").join("..").join("Wallpaper.webp"),
            ]
        })
        .collect::<Vec<_>>();

    // Packaged installs place the first visual stage under the data prefix,
    // which is not necessarily adjacent to the executable. Keep these paths
    // late in the list so a developer checkout still wins, while the .deb,
    // Arch package and Bash installer all find the same Tahoe wallpaper.
    if let Some(interface_dir) = std::env::var_os("ROUCH_INTERFACE_DIR") {
        candidates.push(std::path::PathBuf::from(interface_dir).join("Wallpaper.webp"));
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        candidates.push(
            std::path::PathBuf::from(data_home)
                .join("rouch")
                .join("interface-stage1")
                .join("Wallpaper.webp"),
        );
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates
            .push(std::path::PathBuf::from(home).join(".local/share/rouch/interface-stage1/Wallpaper.webp"));
    }
    candidates.extend([
        std::path::PathBuf::from("/usr/local/share/rouch/interface-stage1/Wallpaper.webp"),
        std::path::PathBuf::from("/usr/share/rouch/interface-stage1/Wallpaper.webp"),
    ]);
    candidates
}

/// The first wallpaper candidate that exists on disk.
pub fn find_wallpaper() -> Option<std::path::PathBuf> {
    wallpaper_candidates().into_iter().find(|path| path.is_file())
}
