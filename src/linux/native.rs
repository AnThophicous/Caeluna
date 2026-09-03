//! Minimal native DRM/KMS frame pipeline.
//!
//! This module owns the part of a native compositor that must be backed by
//! the physical GPU: one `DrmSurface`, a GBM allocator, an EGL display and a
//! GLES renderer.  It deliberately does not own the Wayland server or Rouch's
//! desktop state.  The caller supplies Smithay render elements and drives the
//! pipeline from its event loop.

use std::fmt;

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            dmabuf::Dmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{DrmDevice, DrmDeviceFd, GbmBufferedSurface},
        egl::{EGLContext, EGLDisplay},
        renderer::{Bind, Color32F, Frame, Renderer, element::RenderElement, gles::GlesRenderer},
    },
    reexports::drm::control::{Mode, connector, crtc},
    utils::{Physical, Rectangle, Scale, Size, Transform},
};

use super::NativeOutputConfig;

type BufferedSurface = GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, ()>;

/// Failure from a real native rendering operation.
#[derive(Debug, Clone)]
pub(crate) struct NativePipelineError {
    detail: String,
}

impl NativePipelineError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for NativePipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for NativePipelineError {}

/// One physical output and its GBM/EGL/GLES scanout pipeline.
pub(crate) struct NativeFramePipeline {
    renderer: GlesRenderer,
    gbm: GbmDevice<DrmDeviceFd>,
    surface: Option<BufferedSurface>,
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
    size: Size<i32, Physical>,
    queued_frames: u64,
    presented_frames: u64,
    frame_pending: bool,
}

impl fmt::Debug for NativeFramePipeline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeFramePipeline")
            .field("connector", &self.connector)
            .field("crtc", &self.crtc)
            .field("mode", &self.mode)
            .field("size", &self.size)
            .field("surface_active", &self.surface.is_some())
            .field("queued_frames", &self.queued_frames)
            .field("presented_frames", &self.presented_frames)
            .finish_non_exhaustive()
    }
}

impl NativeFramePipeline {
    /// Create the physical scanout resources for one connector/CRTC/mode.
    pub(crate) fn new(
        device: &mut DrmDevice,
        config: NativeOutputConfig,
    ) -> Result<Self, NativePipelineError> {
        let surface = device
            .create_surface(config.crtc, config.mode, &[config.connector])
            .map_err(|error| NativePipelineError::new(format!("DRM surface: {error}")))?;

        let drm_fd = device.device_fd().clone();
        let gbm = GbmDevice::new(drm_fd.clone())
            .map_err(|error| NativePipelineError::new(format!("GBM device: {error}")))?;
        let display = unsafe { EGLDisplay::new(gbm.clone()) }
            .map_err(|error| NativePipelineError::new(format!("EGL display: {error}")))?;
        let context = EGLContext::new(&display)
            .map_err(|error| NativePipelineError::new(format!("EGL context: {error}")))?;
        let mut renderer = unsafe { GlesRenderer::new(context) }
            .map_err(|error| NativePipelineError::new(format!("GLES renderer: {error}")))?;

        let renderer_formats = <GlesRenderer as Bind<Dmabuf>>::supported_formats(&renderer)
            .ok_or_else(|| NativePipelineError::new("GLES renderer did not report dmabuf formats"))?;
        if renderer_formats.iter().next().is_none() {
            return Err(NativePipelineError::new(
                "GLES renderer reported no dmabuf render formats",
            ));
        }

        let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let exporter = surface;
        let buffered = GbmBufferedSurface::new(
            exporter,
            allocator,
            &[Fourcc::Argb8888, Fourcc::Xrgb8888],
            renderer_formats,
        )
        .map_err(|error| NativePipelineError::new(format!("GBM scanout buffers: {error}")))?;

        let (width, height) = config.mode.size();
        Ok(Self {
            renderer,
            gbm,
            surface: Some(buffered),
            connector: config.connector,
            crtc: config.crtc,
            mode: config.mode,
            size: (width as i32, height as i32).into(),
            queued_frames: 0,
            presented_frames: 0,
            frame_pending: false,
        })
    }

    pub(crate) fn config(&self) -> NativeOutputConfig {
        NativeOutputConfig {
            connector: self.connector,
            crtc: self.crtc,
            mode: self.mode,
        }
    }

    pub(crate) fn crtc(&self) -> crtc::Handle {
        self.crtc
    }

    pub(crate) fn size(&self) -> Size<i32, Physical> {
        self.size
    }

    pub(crate) fn queued_frames(&self) -> u64 {
        self.queued_frames
    }

    pub(crate) fn presented_frames(&self) -> u64 {
        self.presented_frames
    }

    pub(crate) fn can_render(&self) -> bool {
        self.surface.is_some() && !self.frame_pending
    }

    /// Stop using the current scanout surface before DRM master is released.
    pub(crate) fn pause(&mut self) {
        // Dropping the buffered surface cancels our ownership of its swapchain
        // and its DRM surface.  The underlying DrmDevice is paused by the
        // caller immediately afterwards, so no ioctl is attempted here.
        self.surface.take();
        self.frame_pending = false;
    }

    /// Recreate the DRM surface and buffer swapchain after seat activation.
    pub(crate) fn activate(&mut self, device: &mut DrmDevice) -> Result<(), NativePipelineError> {
        if self.surface.is_some() {
            return Ok(());
        }

        let drm_surface = device
            .create_surface(self.crtc, self.mode, &[self.connector])
            .map_err(|error| NativePipelineError::new(format!("DRM surface reactivation: {error}")))?;
        let allocator = GbmAllocator::new(
            self.gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let renderer_formats = <GlesRenderer as Bind<Dmabuf>>::supported_formats(&self.renderer)
            .ok_or_else(|| NativePipelineError::new("GLES renderer lost dmabuf formats"))?;
        let buffered = GbmBufferedSurface::new(
            drm_surface,
            allocator,
            &[Fourcc::Argb8888, Fourcc::Xrgb8888],
            renderer_formats,
        )
        .map_err(|error| NativePipelineError::new(format!("GBM reactivation buffers: {error}")))?;
        self.surface = Some(buffered);
        self.frame_pending = false;
        Ok(())
    }

    /// Render a frame after constructing the scene against this renderer.
    ///
    /// Scene construction must happen after the GBM framebuffer is bound so
    /// client buffers and compositor chrome use the exact same GLES context.
    pub(crate) fn render_frame_with<E, F>(
        &mut self,
        clear_color: Color32F,
        build: F,
    ) -> Result<(), NativePipelineError>
    where
        E: RenderElement<GlesRenderer>,
        F: FnOnce(&mut GlesRenderer) -> Result<Vec<E>, String>,
    {
        if self.frame_pending {
            return Err(NativePipelineError::new(
                "native scanout is waiting for the previous page-flip",
            ));
        }
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| NativePipelineError::new("native scanout surface is paused"))?;
        let (mut dmabuf, _buffer_age) = surface
            .next_buffer()
            .map_err(|error| NativePipelineError::new(format!("acquire GBM buffer: {error}")))?;
        let full_damage = Rectangle::from_size(self.size);
        let scale = Scale::from(1.0);

        let sync = {
            let mut framebuffer = self
                .renderer
                .bind(&mut dmabuf)
                .map_err(|error| NativePipelineError::new(format!("bind GBM buffer: {error}")))?;
            let elements = build(&mut self.renderer).map_err(NativePipelineError::new)?;
            let mut frame = self
                .renderer
                .render(&mut framebuffer, self.size, Transform::Normal)
                .map_err(|error| NativePipelineError::new(format!("begin GLES frame: {error}")))?;
            frame
                .clear(clear_color, &[full_damage])
                .map_err(|error| NativePipelineError::new(format!("clear GLES frame: {error}")))?;

            for element in &elements {
                element
                    .draw(
                        &mut frame,
                        element.src(),
                        element.geometry(scale),
                        &[full_damage],
                        &element.opaque_regions(scale),
                    )
                    .map_err(|error| NativePipelineError::new(format!("draw render element: {error}")))?;
            }

            frame
                .finish()
                .map_err(|error| NativePipelineError::new(format!("finish GLES frame: {error}")))?
        };

        surface
            .queue_buffer(Some(sync), Some(vec![full_damage]), ())
            .map_err(|error| NativePipelineError::new(format!("queue DRM frame: {error}")))?;
        self.frame_pending = true;
        self.queued_frames = self.queued_frames.saturating_add(1);
        Ok(())
    }

    /// Render and queue a frame.  The first queue performs the pending modeset;
    /// later queues use DRM page-flip and are completed by `frame_submitted`.
    pub(crate) fn render_frame<E>(
        &mut self,
        elements: &[E],
        clear_color: Color32F,
    ) -> Result<(), NativePipelineError>
    where
        E: RenderElement<GlesRenderer>,
    {
        if self.frame_pending {
            return Err(NativePipelineError::new(
                "native scanout is waiting for the previous page-flip",
            ));
        }
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| NativePipelineError::new("native scanout surface is paused"))?;
        let (mut dmabuf, _buffer_age) = surface
            .next_buffer()
            .map_err(|error| NativePipelineError::new(format!("acquire GBM buffer: {error}")))?;
        let full_damage = Rectangle::from_size(self.size);
        let scale = Scale::from(1.0);

        let sync = {
            let mut framebuffer = self
                .renderer
                .bind(&mut dmabuf)
                .map_err(|error| NativePipelineError::new(format!("bind GBM buffer: {error}")))?;
            let mut frame = self
                .renderer
                .render(&mut framebuffer, self.size, Transform::Normal)
                .map_err(|error| NativePipelineError::new(format!("begin GLES frame: {error}")))?;
            frame
                .clear(clear_color, &[full_damage])
                .map_err(|error| NativePipelineError::new(format!("clear GLES frame: {error}")))?;

            for element in elements {
                element
                    .draw(
                        &mut frame,
                        element.src(),
                        element.geometry(scale),
                        &[full_damage],
                        &element.opaque_regions(scale),
                    )
                    .map_err(|error| NativePipelineError::new(format!("draw render element: {error}")))?;
            }

            frame
                .finish()
                .map_err(|error| NativePipelineError::new(format!("finish GLES frame: {error}")))?
        };

        surface
            .queue_buffer(Some(sync), Some(vec![full_damage]), ())
            .map_err(|error| NativePipelineError::new(format!("queue DRM frame: {error}")))?;
        self.frame_pending = true;
        self.queued_frames = self.queued_frames.saturating_add(1);
        Ok(())
    }

    /// Complete the swapchain transition belonging to a DRM vblank event.
    pub(crate) fn frame_submitted(&mut self, crtc: crtc::Handle) -> Result<bool, NativePipelineError> {
        if crtc != self.crtc {
            return Ok(false);
        }
        let Some(surface) = self.surface.as_mut() else {
            return Ok(false);
        };
        let submitted = surface
            .frame_submitted()
            .map_err(|error| NativePipelineError::new(format!("complete DRM frame: {error}")))?;
        if submitted.is_some() {
            self.frame_pending = false;
            self.presented_frames = self.presented_frames.saturating_add(1);
        }
        Ok(submitted.is_some())
    }
}
