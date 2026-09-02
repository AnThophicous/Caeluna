//! Linux-side rendering policy for Liquid Glass and low-end machines.
//!
//! This module is intentionally a pure decision layer. It gives the real
//! renderer one bounded answer for material, DPR, animation and redraw
//! cadence, but it does not pretend to measure a GPU or to turn the current
//! Smithay/Winit GLES surface into Vulkan. The nested path currently enables
//! VSync on its OpenGL/EGL surface; a native DRM/Vulkan integration can
//! consume these same decisions later.

#![allow(dead_code)]

use crate::design::{
    BackdropQuality, GlassRecipe, GlassSurface, MaterialBudget, PerformanceProfile, glass_recipe,
};

#[allow(unused_imports)]
pub(crate) use crate::graphics::{
    BackendDiagnostic, BackendProbe, BackendState, DiagnosticCode, GraphicsBackend, GraphicsConfig,
    GraphicsDecision, GraphicsSelectionError, GraphicsSelector, PerformanceMode, PerformancePolicy,
    RendererBackend, SelectionState, select_backend, select_backend_with_software, select_from_availability,
};

/// Physical output dimensions used for fill-rate estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputMetrics {
    pub width: u32,
    pub height: u32,
    /// Host-reported scale in thousandths (1000 = 1x).
    pub observed_dpr_milli: u16,
}

/// Redraw cadence. Both variants are compositor-paced; there is no free
/// running animation loop in the low-end policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedrawCadence {
    VSync,
    VSyncCapped { target_fps: u16 },
}

impl RedrawCadence {
    pub const fn target_fps(self) -> u16 {
        match self {
            Self::VSync => 0,
            Self::VSyncCapped { target_fps } => target_fps,
        }
    }

    pub const fn interval_ms(self) -> u16 {
        match self.target_fps() {
            0 => 0,
            fps => 1000 / fps,
        }
    }
}

/// The applied material decision, including honest cost proxies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MaterialDecision {
    pub requested: GlassRecipe,
    pub applied: GlassRecipe,
    pub degraded_to_opaque: bool,
    pub sampled_backdrop_pixels: u64,
    pub estimated_cache_bytes: u64,
}

/// Renderer-facing policy for one output and one shell surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderPolicy {
    pub profile: PerformanceProfile,
    pub budget: MaterialBudget,
    pub dpr_milli: u16,
    pub cadence: RedrawCadence,
    pub animation_cap_ms: u16,
    pub material: MaterialDecision,
}

impl RenderPolicy {
    /// Resolve one bounded Liquid Glass surface.
    ///
    /// `OutputMetrics::width` and `height` are physical pixels. The DPR cap
    /// is returned separately so a renderer can reduce its logical raster
    /// scale without accidentally double-counting physical dimensions here.
    pub(crate) fn for_output(
        profile: PerformanceProfile,
        reduce_transparency: bool,
        output: OutputMetrics,
        surface: GlassSurface,
        active_glass_surfaces: u8,
    ) -> Self {
        let budget = MaterialBudget::for_profile(profile);
        let requested = glass_recipe(profile, reduce_transparency, surface);
        let fits = budget.allows_backdrop(requested, output.width, output.height, active_glass_surfaces);
        let applied = if fits {
            requested
        } else {
            glass_recipe(profile, true, surface)
        };

        Self {
            profile,
            budget,
            dpr_milli: budget.clamp_dpr_milli(output.observed_dpr_milli),
            cadence: if budget.target_fps >= 60 {
                RedrawCadence::VSync
            } else {
                RedrawCadence::VSyncCapped {
                    target_fps: budget.target_fps,
                }
            },
            animation_cap_ms: budget.max_animation_ms,
            material: MaterialDecision {
                requested,
                applied,
                degraded_to_opaque: applied != requested,
                sampled_backdrop_pixels: applied.backdrop.sampled_pixels(output.width, output.height),
                estimated_cache_bytes: applied.estimated_cache_bytes(output.width, output.height),
            },
        }
    }

    /// Request a frame only for visible damage or a bounded active animation.
    pub const fn should_request_redraw(self, visible: bool, damaged: bool, animating: bool) -> bool {
        visible && (damaged || (animating && self.animation_cap_ms > 0))
    }

    /// Whether an animation sample is still inside this profile's budget.
    pub const fn animation_is_bounded(self, elapsed_ms: u16) -> bool {
        self.animation_cap_ms > 0 && elapsed_ms < self.animation_cap_ms
    }

    /// Whether a recipe would request a backdrop pass at all.
    pub const fn samples_backdrop(self) -> bool {
        !matches!(self.material.applied.backdrop, BackdropQuality::Disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: OutputMetrics = OutputMetrics {
        width: 1920,
        height: 1080,
        observed_dpr_milli: 2000,
    };

    #[test]
    fn balanced_tahoe_surfaces_keep_bounded_glass_and_cap_dpr() {
        let policy = RenderPolicy::for_output(
            PerformanceProfile::Balanced,
            false,
            HD,
            GlassSurface::ControlCenter,
            1,
        );

        assert_eq!(policy.dpr_milli, 1500);
        assert_eq!(policy.material.applied.backdrop, BackdropQuality::Half);
        assert_eq!(policy.material.sampled_backdrop_pixels, 518_400);
        assert!(policy.material.estimated_cache_bytes <= 8_388_608);
        assert!(matches!(policy.cadence, RedrawCadence::VSync));
    }

    #[test]
    fn large_outputs_degrade_overlay_to_opaque_before_exceeding_budget() {
        let policy = RenderPolicy::for_output(
            PerformanceProfile::Balanced,
            false,
            OutputMetrics {
                width: 3840,
                height: 2160,
                observed_dpr_milli: 1000,
            },
            GlassSurface::Notifications,
            1,
        );

        assert!(policy.material.degraded_to_opaque);
        assert_eq!(policy.material.applied.backdrop, BackdropQuality::Disabled);
        assert!(policy.material.applied.opaque_content);
        assert!(!policy.samples_backdrop());
    }

    #[test]
    fn low_end_is_vsync_capped_and_does_not_redraw_hidden_animation() {
        let policy = RenderPolicy::for_output(PerformanceProfile::LowEnd, false, HD, GlassSurface::Dock, 0);

        assert!(matches!(
            policy.cadence,
            RedrawCadence::VSyncCapped { target_fps: 30 }
        ));
        assert_eq!(policy.animation_cap_ms, 0);
        assert!(!policy.should_request_redraw(false, false, true));
        assert!(!policy.should_request_redraw(true, false, true));
        assert!(policy.should_request_redraw(true, true, false));
    }

    #[test]
    fn reduced_transparency_and_window_chrome_are_opaque() {
        let reduced = RenderPolicy::for_output(PerformanceProfile::Visual, true, HD, GlassSurface::Dock, 1);
        let chrome = RenderPolicy::for_output(
            PerformanceProfile::Visual,
            false,
            HD,
            GlassSurface::WindowChrome,
            1,
        );

        assert!(!reduced.samples_backdrop());
        assert!(reduced.material.applied.opaque_content);
        assert!(!chrome.samples_backdrop());
        assert!(chrome.material.applied.opaque_content);
    }

    #[test]
    fn animation_budget_is_interruptible_and_bounded() {
        let policy =
            RenderPolicy::for_output(PerformanceProfile::Balanced, false, HD, GlassSurface::TopBar, 1);

        assert!(policy.animation_is_bounded(359));
        assert!(!policy.animation_is_bounded(360));
        assert!(policy.should_request_redraw(true, false, true));
    }
}
