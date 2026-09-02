//! Design rules that are cheap to enforce now and independent from the renderer.
//!
//! Caelune should feel like a single system, rather than a collection of copied
//! interfaces: macOS informs behaviour, while Material informs geometry and
//! colour. The renderer will consume these values in later milestones.

/// Identifies the initial visual language and supplied default background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualProfile {
    pub product_name: &'static str,
    pub default_wallpaper: &'static str,
    pub shape_language: &'static str,
}

impl VisualProfile {
    pub const DEFAULT: Self = Self {
        product_name: "Caelune",
        default_wallpaper: "Wallpaper.webp",
        shape_language: "Liquid Material",
    };
}

/// The only shell surfaces allowed to request backdrop sampling.
///
/// Application contents never receive compositor blur automatically. This is
/// both less distracting and much cheaper on integrated GPUs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlassSurface {
    TopBar,
    Dock,
    ControlCenter,
    Notifications,
    GalleryChrome,
    Widget,
    WindowChrome,
}

/// How much detail the blur pass may sample. `Quarter` means that a backdrop
/// is captured at one quarter of the output resolution in each dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackdropQuality {
    Disabled,
    Quarter,
    Half,
}

impl BackdropQuality {
    /// Estimate sampled pixels without allocating or touching the renderer.
    /// `Quarter` and `Half` describe the scale in each dimension, not the
    /// fraction of total pixels.  This makes the fill-rate cost explicit.
    pub const fn sampled_pixels(self, output_width: u32, output_height: u32) -> u64 {
        let pixels = output_width as u64 * output_height as u64;
        match self {
            Self::Disabled => 0,
            Self::Quarter => pixels.div_ceil(16),
            Self::Half => pixels.div_ceil(4),
        }
    }
}

/// User-visible performance profile. The default intentionally favours
/// consistency and battery life over a full-resolution blur effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PerformanceProfile {
    BatterySaver,
    /// A deliberately opaque, low-fill-rate profile for weak integrated GPUs.
    LowEnd,
    #[default]
    Balanced,
    /// A bounded showcase profile; it still never samples full output.
    Quality,
    Visual,
}

/// A small, measurable budget for Liquid Glass and shell redraw work.
///
/// These are policy proxies, not benchmark claims: they bound the amount of
/// work the renderer may request before a device-specific frame-time probe is
/// available.  Values are intentionally expressed as integers so this module
/// stays deterministic and allocation-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterialBudget {
    /// Maximum device scale in thousandths (1500 = 1.5x).
    pub max_dpr_milli: u16,
    /// Maximum sampled backdrop pixels in a frame.
    pub max_backdrop_pixels: u32,
    /// Maximum shell surfaces allowed to sample a backdrop concurrently.
    pub max_glass_surfaces: u8,
    /// Approximate reusable backdrop cache cap in bytes.
    pub max_cached_backdrop_bytes: u32,
    /// Maximum spatial transition duration in milliseconds.
    pub max_animation_ms: u16,
    /// Target frame budget in milliseconds.
    pub frame_budget_ms: u16,
    /// VSync-paced target cap used when the host does not expose a refresh rate.
    pub target_fps: u16,
}

impl MaterialBudget {
    /// Return the budget appropriate for a visual profile.
    pub const fn for_profile(profile: PerformanceProfile) -> Self {
        match profile {
            PerformanceProfile::Quality => Self {
                max_dpr_milli: 1800,
                max_backdrop_pixels: 4_194_304,
                max_glass_surfaces: 4,
                max_cached_backdrop_bytes: 16_777_216,
                max_animation_ms: 700,
                frame_budget_ms: 16,
                target_fps: 60,
            },
            PerformanceProfile::Visual => Self {
                max_dpr_milli: 1500,
                max_backdrop_pixels: 1_048_576,
                max_glass_surfaces: 3,
                max_cached_backdrop_bytes: 8_388_608,
                max_animation_ms: 360,
                frame_budget_ms: 16,
                target_fps: 60,
            },
            PerformanceProfile::Balanced => Self {
                max_dpr_milli: 1500,
                max_backdrop_pixels: 1_048_576,
                max_glass_surfaces: 3,
                max_cached_backdrop_bytes: 8_388_608,
                max_animation_ms: 360,
                frame_budget_ms: 16,
                target_fps: 60,
            },
            PerformanceProfile::LowEnd | PerformanceProfile::BatterySaver => Self {
                max_dpr_milli: 1000,
                max_backdrop_pixels: 0,
                max_glass_surfaces: 0,
                max_cached_backdrop_bytes: 0,
                max_animation_ms: 0,
                frame_budget_ms: 33,
                target_fps: 30,
            },
        }
    }

    /// Cap a host-provided device scale without ever upscaling a 1x output.
    pub const fn clamp_dpr_milli(self, observed_dpr_milli: u16) -> u16 {
        let observed = if observed_dpr_milli < 1000 {
            1000
        } else {
            observed_dpr_milli
        };
        if observed < self.max_dpr_milli {
            observed
        } else {
            self.max_dpr_milli
        }
    }

    /// Whether a recipe fits the per-frame backdrop and concurrency proxy.
    pub const fn allows_backdrop(
        self,
        recipe: GlassRecipe,
        output_width: u32,
        output_height: u32,
        active_glass_surfaces: u8,
    ) -> bool {
        if matches!(recipe.backdrop, BackdropQuality::Disabled) {
            return true;
        }
        recipe.backdrop.sampled_pixels(output_width, output_height) <= self.max_backdrop_pixels as u64
            && active_glass_surfaces <= self.max_glass_surfaces
            && recipe.estimated_cache_bytes(output_width, output_height)
                <= self.max_cached_backdrop_bytes as u64
    }
}

/// A renderer-ready decision for a Liquid Material surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlassRecipe {
    pub backdrop: BackdropQuality,
    pub refraction: bool,
    pub tint_alpha_percent: u8,
    /// Small bounded blur radius; zero means no sampling/filter pass.
    pub blur_radius_px: u8,
    /// One restrained shadow layer, expressed as alpha percent.
    pub shadow_alpha_percent: u8,
    /// Whether this surface may reuse a per-output backdrop cache.
    pub cacheable: bool,
    /// Whether content beneath this surface must remain opaque/readable.
    pub opaque_content: bool,
}

impl GlassRecipe {
    /// Estimate RGBA cache storage for one backdrop, including one scratch
    /// surface for a two-pass bounded blur.  This is a sizing proxy, not a
    /// measured GPU allocation report.
    pub const fn estimated_cache_bytes(self, output_width: u32, output_height: u32) -> u64 {
        if !self.cacheable {
            return 0;
        }
        let pixels = self.backdrop.sampled_pixels(output_width, output_height);
        pixels * 4 * if self.blur_radius_px > 0 { 2 } else { 1 }
    }
}

/// Decide an effect level without ever requesting an expensive full-output
/// backdrop. This is a key constraint for the project's lightweight goal.
pub const fn glass_recipe(
    profile: PerformanceProfile,
    reduce_transparency: bool,
    surface: GlassSurface,
) -> GlassRecipe {
    if reduce_transparency
        || matches!(
            profile,
            PerformanceProfile::BatterySaver | PerformanceProfile::LowEnd
        )
        || matches!(surface, GlassSurface::WindowChrome)
    {
        return GlassRecipe {
            backdrop: BackdropQuality::Disabled,
            refraction: false,
            tint_alpha_percent: 92,
            blur_radius_px: 0,
            shadow_alpha_percent: 0,
            cacheable: false,
            opaque_content: true,
        };
    }

    match (profile, surface) {
        (
            PerformanceProfile::Balanced,
            GlassSurface::ControlCenter
            | GlassSurface::Notifications
            | GlassSurface::GalleryChrome
            | GlassSurface::Widget,
        ) => GlassRecipe {
            backdrop: BackdropQuality::Half,
            refraction: false,
            tint_alpha_percent: 68,
            blur_radius_px: 10,
            shadow_alpha_percent: 18,
            cacheable: true,
            opaque_content: false,
        },
        (
            PerformanceProfile::Visual,
            GlassSurface::ControlCenter
            | GlassSurface::Notifications
            | GlassSurface::GalleryChrome
            | GlassSurface::Widget,
        ) => GlassRecipe {
            backdrop: BackdropQuality::Half,
            refraction: true,
            tint_alpha_percent: 60,
            blur_radius_px: 12,
            shadow_alpha_percent: 22,
            cacheable: true,
            opaque_content: false,
        },
        (PerformanceProfile::Visual, _) => GlassRecipe {
            backdrop: BackdropQuality::Quarter,
            refraction: false,
            tint_alpha_percent: 70,
            blur_radius_px: 8,
            shadow_alpha_percent: 16,
            cacheable: true,
            opaque_content: false,
        },
        (_, _) => GlassRecipe {
            backdrop: BackdropQuality::Quarter,
            refraction: false,
            tint_alpha_percent: 72,
            blur_radius_px: 8,
            shadow_alpha_percent: 14,
            cacheable: true,
            opaque_content: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_saver_never_blurs() {
        for surface in [
            GlassSurface::TopBar,
            GlassSurface::Dock,
            GlassSurface::ControlCenter,
            GlassSurface::Notifications,
            GlassSurface::GalleryChrome,
            GlassSurface::Widget,
            GlassSurface::WindowChrome,
        ] {
            let recipe = glass_recipe(PerformanceProfile::BatterySaver, false, surface);
            assert_eq!(recipe.backdrop, BackdropQuality::Disabled);
            assert!(!recipe.refraction);
        }
    }

    #[test]
    fn accessibility_setting_overrides_visual_profile() {
        let recipe = glass_recipe(PerformanceProfile::Visual, true, GlassSurface::ControlCenter);
        assert_eq!(recipe.backdrop, BackdropQuality::Disabled);
        assert_eq!(recipe.tint_alpha_percent, 92);
        assert!(recipe.opaque_content);
    }

    #[test]
    fn visual_mode_keeps_blur_bounded() {
        let recipe = glass_recipe(PerformanceProfile::Visual, false, GlassSurface::Widget);
        assert_eq!(recipe.backdrop, BackdropQuality::Half);
        assert!(recipe.refraction);
        assert!(recipe.cacheable);
        assert_eq!(recipe.blur_radius_px, 12);
    }

    #[test]
    fn low_end_is_opaque_and_window_chrome_never_samples() {
        let low_end = glass_recipe(PerformanceProfile::LowEnd, false, GlassSurface::Dock);
        let chrome = glass_recipe(PerformanceProfile::Visual, false, GlassSurface::WindowChrome);

        assert_eq!(low_end.backdrop, BackdropQuality::Disabled);
        assert!(!low_end.cacheable);
        assert!(low_end.opaque_content);
        assert_eq!(chrome.backdrop, BackdropQuality::Disabled);
        assert!(chrome.opaque_content);
    }

    #[test]
    fn balanced_backdrop_and_cache_stay_within_one_megapixel_proxy() {
        let budget = MaterialBudget::for_profile(PerformanceProfile::Balanced);
        let recipe = glass_recipe(PerformanceProfile::Balanced, false, GlassSurface::ControlCenter);

        assert_eq!(recipe.backdrop.sampled_pixels(1920, 1080), 518_400);
        assert!(budget.allows_backdrop(recipe, 1920, 1080, 1));
        assert!(!budget.allows_backdrop(recipe, 3840, 2160, 1));
        assert_eq!(budget.clamp_dpr_milli(2000), 1500);
        assert_eq!(budget.clamp_dpr_milli(900), 1000);
    }

    #[test]
    fn profile_budgets_have_a_measurable_low_end_floor() {
        let low_end = MaterialBudget::for_profile(PerformanceProfile::LowEnd);
        let balanced = MaterialBudget::for_profile(PerformanceProfile::Balanced);

        assert_eq!(low_end.max_backdrop_pixels, 0);
        assert_eq!(low_end.max_cached_backdrop_bytes, 0);
        assert_eq!(low_end.max_animation_ms, 0);
        assert!(balanced.max_dpr_milli > low_end.max_dpr_milli);
        assert!(balanced.target_fps > low_end.target_fps);
    }
}
