//! Winit backend: runs Rouch inside an existing Linux Wayland or X11 session.
//!
//! This is the safe first graphical backend. It provides a real output and GL
//! framebuffer without taking ownership of DRM devices or the user's VT.
//! Pointer input is fully interactive: the traffic lights, title-bar drags and
//! edge resizes all drive the same `WindowManager` rules the dock will use.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
            PointerButtonEvent,
        },
        renderer::{
            ImportAll, ImportMem,
            damage::OutputDamageTracker,
            element::{
                Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            utils::with_renderer_surface_state,
        },
        winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend},
    },
    desktop::{
        WindowSurfaceType,
        utils::{send_frames_surface_tree, under_from_surface_tree},
    },
    input::{
        keyboard::FilterResult,
        pointer::{ButtonEvent, MotionEvent},
    },
    reexports::{
        calloop::{
            EventLoop,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{Display, protocol::wl_surface::WlSurface},
    },
    utils::{Logical, Physical, Point as SmithayPoint, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor,
        shell::xdg::{
            PopupSurface, SurfaceCachedState, ToplevelSurface, XDG_POPUP_ROLE, XDG_TOPLEVEL_ROLE,
            XdgPopupSurfaceData, XdgToplevelSurfaceData,
        },
    },
};
use tracing::{debug, error, info, warn};

use super::{Rouch, input};
use crate::{
    chrome::{ChromeAction, action_at},
    windowing::{Point, Rect, WindowId},
};

/// Double-click window for the title-bar maximize gesture, like macOS.
const DOUBLE_CLICK_MS: u128 = 400;

/// The parent compositor currently keeps the XDG popup list but does not expose
/// a `PopupManager` field to this backend. Until that API is available, this
/// module provides a deliberately small, safe fallback for popup placement,
/// rendering and hit testing. Popup grab tracking/dismissal still belongs in
/// the XDG handler (`linux.rs`) and must be connected there when that API is
/// exposed; duplicating a second grab state here would make grabs diverge.
const MIN_HOST_SIZE: i32 = 1;

/// The millimetre size a display of this pixel resolution would have at 96 DPI.
///
/// One inch is 25.4 mm, so a pixel at 96 DPI is 25.4/96 mm wide.
pub(super) fn physical_size_at_96_dpi(
    size: smithay::utils::Size<i32, Physical>,
) -> smithay::utils::Size<i32, Physical> {
    let millimetres = |pixels: i32| (f64::from(pixels) * 25.4 / 96.0).round() as i32;
    (millimetres(size.w), millimetres(size.h)).into()
}

fn valid_host_size(size: smithay::utils::Size<i32, Physical>) -> bool {
    size.w >= MIN_HOST_SIZE && size.h >= MIN_HOST_SIZE
}

/// Resolve the next focus index while keeping the current window selected
/// when there is no current focus. This is intentionally pure so keyboard
/// input and a future overview surface can share the same cycle rule.
fn next_cycle_index(len: usize, current: Option<usize>, reverse: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        Some(index) if reverse => index.checked_sub(1).unwrap_or(len - 1),
        Some(index) => (index + 1) % len,
        None => 0,
    })
}

/// A commit count shared by the parent compositor. It lets the nested path
/// refresh `DesktopWindow` bounding boxes only after a client commit instead
/// of walking every surface tree on every continuously scheduled frame.
static LAST_GEOMETRY_COMMIT: AtomicU64 = AtomicU64::new(u64::MAX);

#[derive(Clone)]
struct NestedPopupPlacement {
    surface: PopupSurface,
    root: WindowId,
    location: SmithayPoint<i32, Logical>,
}

#[derive(Clone, Copy)]
struct NestedPopupState {
    configured: bool,
    initial_configure_sent: bool,
    committed: bool,
    geometry: Rectangle<i32, Logical>,
    client_geometry: Rectangle<i32, Logical>,
}

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Winit creates the host window first. Its physical size becomes the first
    // Rouch output, which clients receive through wl_output/xdg-output.
    let attributes = smithay::reexports::winit::window::Window::default_attributes()
        .with_inner_size(smithay::reexports::winit::dpi::LogicalSize::new(1280.0, 800.0))
        .with_title("Rouch")
        .with_visible(true);
    let gl_attributes = smithay::backend::egl::context::GlAttributes {
        version: (3, 0),
        profile: None,
        debug: cfg!(debug_assertions),
        // The old bootstrap requested an uncapped loop and then scheduled a
        // redraw after every swap. VSync gives the Balanced policy a real
        // presentation boundary and prevents a compositor-side busy loop.
        vsync: true,
    };
    let (backend, winit) =
        winit::init_from_attributes_with_gl_attr::<GlesRenderer>(attributes, gl_attributes)?;
    let output_size = backend.window_size();

    let mut event_loop: EventLoop<Rouch> = EventLoop::try_new()?;
    let display: Display<Rouch> = Display::new()?;
    let mut state = Rouch::new(
        &mut event_loop,
        display,
        output_size,
        "rouch-nested",
        smithay::utils::Transform::Flipped180,
        // A nested window has no panel to measure, so report the size a 96 DPI
        // display of this resolution would have. Zero would make every toolkit
        // compute a DPI of zero.
        physical_size_at_96_dpi(output_size),
    )?;

    install_winit_backend(&mut event_loop, &mut state, backend, winit)?;

    info!(
        socket = ?state.socket_name,
        language = state.visual_profile.shape_language,
        top_bar_glass = ?state.top_bar_glass,
        seat_available = state.seat.global().is_some(),
        "Rouch nested compositor is ready"
    );
    info!(
        wayland_display = ?state.socket_name,
        "Launch a client with WAYLAND_DISPLAY set to this socket"
    );

    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

fn install_winit_backend(
    event_loop: &mut EventLoop<Rouch>,
    state: &mut Rouch,
    mut backend: WinitGraphicsBackend<GlesRenderer>,
    winit: WinitEventLoop,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = state.output.clone();
    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    // Accelerated clients need the dmabuf global before they connect.
    {
        use smithay::backend::{allocator::dmabuf::Dmabuf, renderer::Bind};
        let formats = <GlesRenderer as Bind<Dmabuf>>::supported_formats(backend.renderer());
        if let Some(formats) = formats {
            state.enable_dmabuf(formats);
        }
    }

    let backend = Rc::new(RefCell::new(backend));
    backend.borrow().window().request_redraw();

    let event_backend = Rc::clone(&backend);
    event_loop.handle().insert_source(winit, move |event, _, state| {
        let mut backend = event_backend.borrow_mut();
        // Client commits, destruction callbacks and host resizes can all
        // leave the protocol objects ahead of the render space. Reconcile at
        // the edge of the event loop so no input path observes stale maps.
        state.reconcile_nested_clients();
        // The idle session is evaluated on every host event, not by a timer:
        // winit wakes us for input and resizes, which is exactly when the
        // blank threshold matters. A dedicated timer source lands with DRM.
        if state.tick_session() {
            debug!("Session went blank after idle");
        }
        match event {
            WinitEvent::Resized { size, .. } => {
                if valid_host_size(size) {
                    state.update_output_size(size);
                    state.reconcile_nested_clients();
                    state.request_redraw();
                    backend.window().request_redraw();
                } else {
                    // Winit may report a zero-sized surface while the host
                    // window is being minimized. Keep the last valid output
                    // and wait for the next non-zero resize instead of
                    // feeding invalid bounds into XDG configure.
                    warn!(?size, "Ignoring zero-sized nested host resize");
                }
            }
            WinitEvent::Input(event) => {
                let needs_redraw = state.process_input_event(event);
                if needs_redraw {
                    state.request_redraw();
                    backend.window().request_redraw();
                }
            }
            WinitEvent::Redraw => {
                state.consume_redraw_request();
                render_frame(state, &mut backend, &mut damage_tracker);
                if state.redraw_needed() {
                    backend.window().request_redraw();
                }
            }
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            WinitEvent::Focus(focused) => {
                state.host_focus_changed(focused);
                state.request_redraw();
                backend.window().request_redraw();
            }
        }
    })?;

    // The PTY reader is a real background reader, while its public API keeps
    // polling non-blocking. A paced timer wakes the host only while the
    // built-in terminal is visible; it slows to one second when closed so an
    // idle desktop does not become a polling loop.
    let timer_backend = Rc::clone(&backend);
    event_loop.handle().insert_source(
        Timer::from_duration(Duration::from_millis(50)),
        move |_, _, state| {
            let changed = state.poll_terminal();
            let game_mode_changed = state.refresh_game_mode_if_due();
            if changed || game_mode_changed {
                state.request_redraw();
                timer_backend.borrow().window().request_redraw();
            }
            TimeoutAction::ToDuration(if state.terminal_ui.open {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(1000)
            })
        },
    )?;

    Ok(())
}

/// Build the complete Rouch scene for a GLES renderer.  The nested backend
/// submits this scene through Winit, while the native backend submits the same
/// elements through its GBM/DRM swapchain.  Keeping scene construction here is
/// what prevents the native session from becoming a separate, feature-poor
/// compositor.
pub(super) fn build_render_elements(
    state: &mut Rouch,
    renderer: &mut GlesRenderer,
    size: smithay::utils::Size<i32, Physical>,
) -> Vec<super::decorations::RouchRenderElements<GlesRenderer>> {
    // Snapshot window state first: element construction borrows the renderer
    // mutably, so all descriptions are collected up front.
    let descriptions: Vec<(WindowId, crate::windowing::Rect, bool, bool, String)> = state
        .windows
        .windows()
        .iter()
        .filter(|window| state.nested_window_renderable(window.id()))
        .map(|window| {
            (
                window.id(),
                window.frame(),
                window.active(),
                window.maximized(),
                window.title().to_owned(),
            )
        })
        .collect();

    // Chrome textures for this frame, one per visible window. The surfaces
    // map is updated and elements created in one pass; each chrome is then
    // spliced right after its client's surfaces.
    let mut chrome: Vec<(
        WindowId,
        smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>,
    )> = {
        let refs: Vec<(WindowId, crate::windowing::Rect, bool, bool, &str)> = descriptions
            .iter()
            .map(|(id, frame, active, maximized, title)| (*id, *frame, *active, *maximized, title.as_str()))
            .collect();
        let samples: HashMap<WindowId, _> = refs
            .iter()
            .filter_map(|(id, ..)| state.animation_sample(*id).map(|s| (*id, s)))
            .collect();
        let sampler = |id: WindowId| samples.get(&id).copied();
        let elements = state.decorations.render_elements(renderer, &refs, &sampler);
        elements
            .into_iter()
            .zip(refs.iter().map(|(id, ..)| *id))
            .map(|(element, id)| (id, element))
            .collect()
    };

    // Animated opacity is snapshotted before the render pass borrows the
    // renderer and state together.
    let alpha_by_id: HashMap<WindowId, f32> = state
        .windows
        .windows()
        .iter()
        .map(|window| {
            (
                window.id(),
                state
                    .animation_sample(window.id())
                    .map(|(_, opacity, _)| opacity)
                    .unwrap_or(1.0),
            )
        })
        .collect();
    let surface_alpha = |id: WindowId| -> f32 { alpha_by_id.get(&id).copied().unwrap_or(1.0) };

    use smithay::backend::renderer::element::AsRenderElements;
    let mut ordered: Vec<super::decorations::RouchRenderElements<GlesRenderer>> = Vec::new();

    // Wallpaper sits beneath everything; the clear colour shows through if
    // it could not be decoded.
    if let Some(wallpaper) = state.wallpaper.as_mut() {
        if let Some(element) =
            wallpaper.render_element::<GlesRenderer>(renderer, crate::windowing::Size::new(size.w, size.h))
        {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    // Walk the space back-to-front and interleave each client's chrome.
    for desktop_window in state.space.elements() {
        let Some(location) = state.space.element_location(desktop_window) else {
            continue;
        };
        let Some(id) = state
            .xdg_windows
            .iter()
            .find(|binding| &binding.desktop == desktop_window)
            .map(|binding| binding.id)
        else {
            continue;
        };
        if !state.nested_window_renderable(id) {
            continue;
        }

        let surface_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            AsRenderElements::<GlesRenderer>::render_elements(
                desktop_window,
                renderer,
                location.to_physical(1),
                smithay::utils::Scale::from(1.0),
                surface_alpha(id),
            );
        ordered.extend(
            surface_elements
                .into_iter()
                .map(super::decorations::RouchRenderElements::Surface),
        );

        if let Some(index) = chrome.iter().position(|(chrome_id, _)| *chrome_id == id) {
            let (_, element) = chrome.swap_remove(index);
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    for placement in state.nested_popup_placements() {
        let alpha = surface_alpha(placement.root);
        let popup_elements: Vec<super::decorations::RouchRenderElements<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                placement.surface.wl_surface(),
                placement.location.to_physical(1),
                smithay::utils::Scale::from(1.0),
                alpha,
                Kind::Unspecified,
            );
        ordered.extend(popup_elements);
    }

    if state.terminal_ui.open {
        if let Some(element) = state.terminal_renderer.element::<GlesRenderer>(
            renderer,
            state.windows.work_area(),
            &state.terminal_ui,
            &state.fonts,
            if state.toggles.battery_saver {
                crate::design::PerformanceProfile::BatterySaver
            } else {
                crate::design::PerformanceProfile::default()
            },
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    // Dock.
    {
        let work_area = state.windows.work_area();
        let pointer = state.seat.get_pointer().map(|pointer| pointer.current_location());
        let pointer = pointer.map(|location| crate::windowing::Point {
            x: location.x.round() as i32,
            y: location.y.round() as i32,
        });
        let hover = pointer.and_then(|point| {
            state
                .dock
                .hit(work_area, point)
                .map(|index| super::dock_render::Hover { index })
        });
        let bounces: Vec<f32> = (0..state.dock.items().len())
            .map(|index| state.dock_bounce(index))
            .collect();
        let bounce_fn = |index: usize| bounces.get(index).copied().unwrap_or(0.0);

        if let Some(element) = state.dock_renderer.render_element::<GlesRenderer>(
            renderer,
            &state.dock,
            work_area,
            pointer,
            hover,
            &bounce_fn,
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    // Welcome overlay.
    match state.welcome {
        Some(crate::welcome::WelcomeStage::FirstRun) => {
            let frame = crate::welcome::WelcomeFrame::elapsed(state.welcome_started.elapsed());
            if let Some(element) = state.welcome_renderer.first_run_element::<GlesRenderer>(
                renderer,
                state.windows.work_area(),
                frame,
            ) {
                ordered.push(super::decorations::RouchRenderElements::Chrome(element));
            }
            if frame.finished {
                state.dismiss_welcome();
            }
        }
        Some(crate::welcome::WelcomeStage::Update) => {
            let frame = crate::welcome::UpdateFrame::elapsed(state.welcome_started.elapsed());
            let notes = crate::welcome::this_release();
            if let Some(element) = state.welcome_renderer.update_element::<GlesRenderer>(
                renderer,
                state.windows.work_area(),
                frame,
                &notes,
            ) {
                ordered.push(super::decorations::RouchRenderElements::Chrome(element));
            }
        }
        _ => {}
    }

    state.refresh_readings();
    state.relayout_shell();

    // Widget rail and its confirmation popup.
    {
        let work_area = state.windows.work_area();
        let board_rect = crate::widgets::WidgetBoard::board_rect(work_area);
        let readings = &state.readings;
        let reading_fn = |kind: crate::widgets::WidgetKind| readings.widget(kind);
        let now_secs = state.now_secs();
        if let Some(element) = state.widget_renderer.board_element::<GlesRenderer>(
            renderer,
            &state.widget_board,
            board_rect,
            &reading_fn,
            now_secs,
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
        if let Some(popup) = state.remove_popup.clone() {
            let anchor = state
                .widget_board
                .slots
                .get(popup.index)
                .map(|slot| slot.rect(board_rect))
                .unwrap_or(board_rect);
            let rect = crate::widgets::popup_rect(anchor, work_area);
            if let Some(element) = state
                .widget_renderer
                .popup_element::<GlesRenderer>(renderer, rect, popup.kind, work_area)
            {
                ordered.push(super::decorations::RouchRenderElements::Chrome(element));
            }
        }
    }

    if state.control_open {
        let work_area = state.windows.work_area();
        let toggles = state.toggles;
        let readings = &state.readings;
        if let Some(element) = state.shell_renderer.control_element::<GlesRenderer>(
            renderer,
            work_area,
            state.control_layout,
            &toggles,
            readings,
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    if state.finder_open {
        let work_area = state.windows.work_area();
        let places: Vec<(String, String)> = crate::finder::sidebar_places()
            .into_iter()
            .map(|(label, path)| (label.to_owned(), path.to_owned()))
            .collect();
        let items: Vec<crate::finder::FinderItem> = state
            .finder
            .filtered(&state.finder_items)
            .into_iter()
            .cloned()
            .collect();
        if let Some(element) =
            state
                .finder_renderer
                .element::<GlesRenderer>(renderer, work_area, &state.finder, &items, &places)
        {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    if state.settings_open {
        let work_area = state.windows.work_area();
        let rows = {
            let profile = crate::user::UserProfile {
                name: state.profile_name(),
                avatar: crate::user::AvatarSource::default(),
            };
            let toggles = state.toggles;
            state
                .settings_backend
                .pane_rows(state.settings_pane, &profile, &toggles)
        };
        let layout = crate::settings::layout(work_area, state.settings_pane, rows.len());
        let profile = crate::user::UserProfile {
            name: state.profile_name(),
            avatar: crate::user::AvatarSource::default(),
        };
        if let Some(element) = state.settings_renderer.element::<GlesRenderer>(
            renderer,
            work_area,
            layout,
            state.settings_pane,
            &rows,
            &profile,
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    if state.app_store_open {
        let work_area = state.windows.work_area();
        if let Some(element) = state.app_store_renderer.element::<GlesRenderer>(
            renderer,
            work_area,
            &state.app_store,
            &state.fonts,
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    if state.launcher_open {
        let work_area = state.windows.work_area();
        if let Some(element) = paint_launcher(
            renderer,
            work_area,
            &state.applications,
            &state.launcher_query,
            &state.fonts,
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    if state.notifications_open || (!state.notifications.is_empty() && state.welcome.is_none()) {
        let now_ms = state.notification_now_ms();
        if let Some(element) = state.notification_renderer.element::<GlesRenderer>(
            renderer,
            state.windows.work_area(),
            &state.notifications,
            now_ms,
            state.notifications_open,
            &state.fonts,
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    if state.setup_open && state.welcome.is_none() {
        if let Some(element) = state.setup_renderer.element::<GlesRenderer>(
            renderer,
            state.windows.work_area(),
            &state.setup_state,
            &state.fonts,
        ) {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    match state.session {
        crate::idle::Session::Rest => {
            if let Some(element) = paint_rest_screen(renderer, state.windows.work_area(), &state.fonts) {
                ordered.push(super::decorations::RouchRenderElements::Chrome(element));
            }
        }
        crate::idle::Session::Lock => {
            let name = state.profile_name();
            if let Some(element) = paint_lock_screen(
                renderer,
                state.windows.work_area(),
                &name,
                &state.password,
                &state.fonts,
            ) {
                ordered.push(super::decorations::RouchRenderElements::Chrome(element));
            }
        }
        _ => {}
    }

    // Top bar closes the stack.
    {
        let work_area = state.windows.work_area();
        let app_name = state.active_app_name();
        let clock = crate::topbar::clock_text(state.now_secs());
        let snapshot = super::shell_render::BarSnapshot {
            layout: state.topbar_layout,
            app_name: &app_name,
            clock: &clock,
            readings: &state.readings,
            distribution: state.distribution,
            unread_notifications: state
                .notifications
                .unread_visible_count(state.notification_now_ms()),
        };
        if let Some(element) = state
            .shell_renderer
            .bar_element::<GlesRenderer>(renderer, work_area, &snapshot)
        {
            ordered.push(super::decorations::RouchRenderElements::Chrome(element));
        }
    }

    ordered
}

fn render_frame(
    state: &mut Rouch,
    backend: &mut WinitGraphicsBackend<GlesRenderer>,
    damage_tracker: &mut OutputDamageTracker,
) {
    state.refresh_animations();
    state.reconcile_nested_clients();

    // A blanked session renders nothing at all: the deepest power saving a
    // compositor can reach without owning DRM.
    if state.output_blank() {
        return;
    }

    let size = backend.window_size();
    let damage = Rectangle::from_size(size);

    let render_result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // Snapshot window state first: element construction borrows the
        // renderer mutably, so all descriptions are collected up front.
        // A window mid-minimize stays visible until its transition settles.
        let descriptions: Vec<(WindowId, crate::windowing::Rect, bool, bool, String)> = state
            .windows
            .windows()
            .iter()
            .filter(|window| state.nested_window_renderable(window.id()))
            .map(|window| {
                (
                    window.id(),
                    window.frame(),
                    window.active(),
                    window.maximized(),
                    window.title().to_owned(),
                )
            })
            .collect();

        {
            let (renderer, mut framebuffer) = backend.bind()?;

            // Chrome textures for this frame, one per visible window. The
            // surfaces map is updated and elements created in one pass; each
            // chrome is then spliced right after its client's surfaces.
            let mut chrome: Vec<(
                WindowId,
                smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>,
            )> = {
                let refs: Vec<(WindowId, crate::windowing::Rect, bool, bool, &str)> = descriptions
                    .iter()
                    .map(|(id, frame, active, maximized, title)| {
                        (*id, *frame, *active, *maximized, title.as_str())
                    })
                    .collect();
                // Snapshot the animation state up front: rendering borrows
                // the renderer and decorations mutably at the same time.
                let samples: HashMap<WindowId, _> = refs
                    .iter()
                    .filter_map(|(id, ..)| state.animation_sample(*id).map(|s| (*id, s)))
                    .collect();
                let sampler = |id: WindowId| samples.get(&id).copied();
                let elements = state.decorations.render_elements(renderer, &refs, &sampler);
                elements
                    .into_iter()
                    .zip(refs.iter().map(|(id, ..)| *id))
                    .map(|(element, id)| (id, element))
                    .collect()
            };

            // Animated opacity for client surfaces this frame, snapshotted
            // before the render pass borrows the renderer and state.
            let alpha_by_id: HashMap<WindowId, f32> = state
                .windows
                .windows()
                .iter()
                .map(|window| {
                    (
                        window.id(),
                        state
                            .animation_sample(window.id())
                            .map(|(_, opacity, _)| opacity)
                            .unwrap_or(1.0),
                    )
                })
                .collect();
            let surface_alpha = |id: WindowId| -> f32 { alpha_by_id.get(&id).copied().unwrap_or(1.0) };

            // Walk the space back-to-front, interleaving each client's
            // surface elements with its own chrome, so the traffic lights are
            // always drawn above their client but below the next window.
            use smithay::backend::renderer::element::AsRenderElements;

            let mut ordered: Vec<super::decorations::RouchRenderElements<GlesRenderer>> = Vec::new();

            // The wallpaper sits beneath everything; the deep ocean clear
            // colour shows through if it could not be decoded.
            if let Some(wallpaper) = state.wallpaper.as_mut() {
                if let Some(element) = wallpaper
                    .render_element::<GlesRenderer>(renderer, crate::windowing::Size::new(size.w, size.h))
                {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            for desktop_window in state.space.elements() {
                let Some(location) = state.space.element_location(desktop_window) else {
                    continue;
                };
                let Some(id) = state
                    .xdg_windows
                    .iter()
                    .find(|binding| &binding.desktop == desktop_window)
                    .map(|binding| binding.id)
                else {
                    // The space can briefly contain an element while a
                    // destruction callback is being delivered. It is not a
                    // renderable client anymore, so skip it safely.
                    continue;
                };
                if !state.nested_window_renderable(id) {
                    continue;
                }

                let surface_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    AsRenderElements::<GlesRenderer>::render_elements(
                        desktop_window,
                        renderer,
                        location.to_physical(1),
                        smithay::utils::Scale::from(1.0),
                        surface_alpha(id),
                    );
                ordered.extend(
                    surface_elements
                        .into_iter()
                        .map(super::decorations::RouchRenderElements::Surface),
                );

                if let Some(index) = chrome
                    .iter()
                    .position(|(chrome_id, _)| Some(*chrome_id) == Some(id))
                {
                    let (_, element) = chrome.swap_remove(index);
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // PopupManager is intentionally not recreated here. The XDG
            // handler owns the authoritative popup list; this safe fallback
            // only walks already-configured popup trees and places them above
            // their mapped toplevels. It keeps menus/popovers usable while the
            // native WindowManager integration exposes popup tracking.
            for placement in state.nested_popup_placements() {
                let alpha = surface_alpha(placement.root);
                let popup_elements: Vec<super::decorations::RouchRenderElements<GlesRenderer>> =
                    render_elements_from_surface_tree(
                        renderer,
                        placement.surface.wl_surface(),
                        placement.location.to_physical(1),
                        smithay::utils::Scale::from(1.0),
                        alpha,
                        Kind::Unspecified,
                    );
                ordered.extend(popup_elements);
            }

            // The built-in terminal is a shell-owned app surface. It is
            // composited above Wayland clients and below the dock, matching
            // the same z-order as a normal macOS application window.
            if state.terminal_ui.open {
                if let Some(element) = state.terminal_renderer.element::<GlesRenderer>(
                    renderer,
                    state.windows.work_area(),
                    &state.terminal_ui,
                    &state.fonts,
                    if state.toggles.battery_saver {
                        crate::design::PerformanceProfile::BatterySaver
                    } else {
                        crate::design::PerformanceProfile::default()
                    },
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // The dock rides above all windows, like macOS.
            {
                let work_area = state.windows.work_area();
                let pointer = state.seat.get_pointer().map(|pointer| pointer.current_location());
                let pointer = pointer.map(|location| crate::windowing::Point {
                    x: location.x.round() as i32,
                    y: location.y.round() as i32,
                });
                let hover = pointer.and_then(|point| {
                    state
                        .dock
                        .hit(work_area, point)
                        .map(|index| super::dock_render::Hover { index })
                });

                // Snapshot bounce phases; the closure borrows state while
                // the renderer is borrowed mutably too.
                let bounces: Vec<f32> = (0..state.dock.items().len())
                    .map(|index| state.dock_bounce(index))
                    .collect();
                let bounce_fn = |index: usize| bounces.get(index).copied().unwrap_or(0.0);

                if let Some(element) = state.dock_renderer.render_element::<GlesRenderer>(
                    renderer,
                    &state.dock,
                    work_area,
                    pointer,
                    hover,
                    &bounce_fn,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // The welcome experience overlays everything while it plays.
            match state.welcome {
                Some(crate::welcome::WelcomeStage::FirstRun) => {
                    let frame = crate::welcome::WelcomeFrame::elapsed(state.welcome_started.elapsed());
                    if let Some(element) = state.welcome_renderer.first_run_element::<GlesRenderer>(
                        renderer,
                        state.windows.work_area(),
                        frame,
                    ) {
                        ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                    }
                    if frame.finished {
                        state.dismiss_welcome();
                    }
                }
                Some(crate::welcome::WelcomeStage::Update) => {
                    let frame = crate::welcome::UpdateFrame::elapsed(state.welcome_started.elapsed());
                    let notes = crate::welcome::this_release();
                    if let Some(element) = state.welcome_renderer.update_element::<GlesRenderer>(
                        renderer,
                        state.windows.work_area(),
                        frame,
                        &notes,
                    ) {
                        ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                    }
                }
                _ => {}
            }

            // Hardware readings refresh on a paced timer, never per frame.
            state.refresh_readings();
            state.relayout_shell();

            // The widget rail sits above windows, right of the work area.
            {
                let work_area = state.windows.work_area();
                let board_rect = crate::widgets::WidgetBoard::board_rect(work_area);
                let readings = &state.readings;
                let reading_fn = |kind: crate::widgets::WidgetKind| readings.widget(kind);
                let now_secs = state.now_secs();
                if let Some(element) = state.widget_renderer.board_element::<GlesRenderer>(
                    renderer,
                    &state.widget_board,
                    board_rect,
                    &reading_fn,
                    now_secs,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }

                // The removal confirmation popup rides above the rail.
                if let Some(popup) = state.remove_popup.clone() {
                    let anchor = state
                        .widget_board
                        .slots
                        .get(popup.index)
                        .map(|slot| slot.rect(board_rect))
                        .unwrap_or(board_rect);
                    let rect = crate::widgets::popup_rect(anchor, work_area);
                    if let Some(element) = state
                        .widget_renderer
                        .popup_element::<GlesRenderer>(renderer, rect, popup.kind, work_area)
                    {
                        ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                    }
                }
            }

            // The control centre dropdown, above everything but the bar.
            if state.control_open {
                let work_area = state.windows.work_area();
                let toggles = state.toggles;
                let readings = &state.readings;
                if let Some(element) = state.shell_renderer.control_element::<GlesRenderer>(
                    renderer,
                    work_area,
                    state.control_layout,
                    &toggles,
                    readings,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // The Finder window, above the dock like a macOS app window.
            if state.finder_open {
                let work_area = state.windows.work_area();
                let places: Vec<(String, String)> = crate::finder::sidebar_places()
                    .into_iter()
                    .map(|(label, path)| (label.to_owned(), path))
                    .collect();
                let items: Vec<crate::finder::FinderItem> = state
                    .finder
                    .filtered(&state.finder_items)
                    .into_iter()
                    .cloned()
                    .collect();
                if let Some(element) = state.finder_renderer.element::<GlesRenderer>(
                    renderer,
                    work_area,
                    &state.finder,
                    &items,
                    &places,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // The Settings window.
            if state.settings_open {
                let work_area = state.windows.work_area();
                let rows = {
                    let profile = crate::user::UserProfile {
                        name: state.profile_name(),
                        avatar: crate::user::AvatarSource::default(),
                    };
                    let toggles = state.toggles;
                    state
                        .settings_backend
                        .pane_rows(state.settings_pane, &profile, &toggles)
                };
                let layout = crate::settings::layout(work_area, state.settings_pane, rows.len());
                let profile = crate::user::UserProfile {
                    name: state.profile_name(),
                    avatar: crate::user::AvatarSource::default(),
                };
                if let Some(element) = state.settings_renderer.element::<GlesRenderer>(
                    renderer,
                    work_area,
                    layout,
                    state.settings_pane,
                    &rows,
                    &profile,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // The native Flatpak/Flathub gallery is a compositor-owned app
            // surface, so it stays available even when no client application
            // is installed yet.
            if state.app_store_open {
                let work_area = state.windows.work_area();
                if let Some(element) = state.app_store_renderer.element::<GlesRenderer>(
                    renderer,
                    work_area,
                    &state.app_store,
                    &state.fonts,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // The application launcher sheet, over apps but under the bar.
            if state.launcher_open {
                let work_area = state.windows.work_area();
                if let Some(element) = paint_launcher(
                    renderer,
                    work_area,
                    &state.applications,
                    &state.launcher_query,
                    &state.fonts,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // Recent notifications toast above app surfaces; opening the
            // centre uses the same renderer with a taller, grouped panel.
            if state.notifications_open || (!state.notifications.is_empty() && state.welcome.is_none()) {
                let now_ms = state.notification_now_ms();
                if let Some(element) = state.notification_renderer.element::<GlesRenderer>(
                    renderer,
                    state.windows.work_area(),
                    &state.notifications,
                    now_ms,
                    state.notifications_open,
                    &state.fonts,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // Setup is shown after the release/welcome card and remains the
            // topmost recoverable shell surface until the required stages are
            // accounted for.
            if state.setup_open && state.welcome.is_none() {
                if let Some(element) = state.setup_renderer.element::<GlesRenderer>(
                    renderer,
                    state.windows.work_area(),
                    &state.setup_state,
                    &state.fonts,
                ) {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // The rest and lock screens sit above everything.
            match state.session {
                crate::idle::Session::Rest => {
                    if let Some(element) =
                        paint_rest_screen(renderer, state.windows.work_area(), &state.fonts)
                    {
                        ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                    }
                }
                crate::idle::Session::Lock => {
                    let name = state.profile_name();
                    if let Some(element) = paint_lock_screen(
                        renderer,
                        state.windows.work_area(),
                        &name,
                        &state.password,
                        &state.fonts,
                    ) {
                        ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                    }
                }
                _ => {}
            }

            // The top bar closes the stack, like the macOS menu bar.
            {
                let work_area = state.windows.work_area();
                let app_name = state.active_app_name();
                let clock = crate::topbar::clock_text(state.now_secs());
                let distribution = state.distribution;
                let readings = &state.readings;
                let layout = state.topbar_layout;
                let snapshot = super::shell_render::BarSnapshot {
                    layout,
                    app_name: &app_name,
                    clock: &clock,
                    readings,
                    distribution,
                    unread_notifications: state
                        .notifications
                        .unread_visible_count(state.notification_now_ms()),
                };
                if let Some(element) = state
                    .shell_renderer
                    .bar_element::<GlesRenderer>(renderer, work_area, &snapshot)
                {
                    ordered.push(super::decorations::RouchRenderElements::Chrome(element));
                }
            }

            // The clear colour mirrors the ocean palette of the wallpaper.
            damage_tracker.render_output::<_, GlesRenderer>(
                renderer,
                &mut framebuffer,
                0,
                &ordered,
                [0.015, 0.035, 0.095, 1.0],
            )?;
        }
        backend.submit(Some(&[damage]))?;
        Ok(())
    })();

    if let Err(error) = render_result {
        error!(?error, "Could not render nested Rouch frame");
        return;
    }

    let output = state.output.clone();
    let frame_time = state.started_at.elapsed();
    for window in state.space.elements() {
        window.send_frame(&output, frame_time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    for placement in state.nested_popup_placements() {
        if placement.surface.alive() {
            send_frames_surface_tree(
                placement.surface.wl_surface(),
                &output,
                frame_time,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
    }
    state.space.refresh();

    if let Err(error) = state.display_handle.flush_clients() {
        warn!(?error, "Could not flush frame callbacks to Wayland clients");
    }
}

/// Deliver frame callbacks after a native DRM page-flip was queued. This is
/// shared with the Winit path so Wayland clients observe the same frame
/// pacing regardless of which physical backend owns scanout.
pub(super) fn complete_native_frame(state: &mut Rouch) {
    let output = state.output.clone();
    let frame_time = state.started_at.elapsed();
    for window in state.space.elements() {
        window.send_frame(&output, frame_time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    for placement in state.nested_popup_placements() {
        if placement.surface.alive() {
            send_frames_surface_tree(
                placement.surface.wl_surface(),
                &output,
                frame_time,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
    }
    state.space.refresh();

    if let Err(error) = state.display_handle.flush_clients() {
        warn!(
            ?error,
            "Could not flush native frame callbacks to Wayland clients"
        );
    }
}

impl Rouch {
    /// Repair the boundary between protocol objects and Smithay's render
    /// space. XDG commits can arrive while a host event is being processed,
    /// and destruction callbacks are not guaranteed to be observed before a
    /// subsequent input event. Every operation below is idempotent so an
    /// incomplete client state degrades to an unmapped, unfocused surface.
    pub(super) fn reconcile_nested_clients(&mut self) {
        let dead_ids: Vec<WindowId> = self
            .xdg_windows
            .iter()
            .filter(|binding| !binding.surface.alive())
            .map(|binding| binding.id)
            .collect();
        let had_dead_drag = self.active_drag.is_some_and(|id| dead_ids.contains(&id));

        for id in &dead_ids {
            let id = *id;
            let Some(index) = self.xdg_windows.iter().position(|binding| binding.id == id) else {
                continue;
            };
            let binding = self.xdg_windows.remove(index);
            self.space.unmap_elem(&binding.desktop);
            self.decorations.forget(id);
            self.animations.remove(&id);

            if let Some(app_id) = self.window_apps.remove(&id) {
                let remaining = self.window_apps.values().filter(|app| **app == app_id).count();
                self.dock.window_unmapped(&app_id, remaining);
            }
            self.windows.remove(id);
            warn!(
                window_id = id.raw(),
                "Recovered an XDG toplevel whose client failed"
            );
        }

        if had_dead_drag {
            self.active_drag = None;
            self.last_grab_serial = None;
            if let Some(pointer) = self.seat.get_pointer() {
                if pointer.is_grabbed() {
                    // A failed client must not leave a move/resize grab
                    // holding the seat forever. The grab's `unset` callback
                    // reconfigures the remaining windows.
                    pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), 0);
                }
            }
        }

        if !dead_ids.is_empty() {
            // Keep the protocol state of the surviving clients coherent after
            // removing a dead window. The custom space sync below immediately
            // removes any not-yet-ready elements reconfigure_windows maps.
            self.reconfigure_windows();
        }

        self.popups.retain(|popup| {
            let alive = popup.alive();
            if !alive {
                debug!("Discarding a dead XDG popup from the nested fallback");
            }
            alive
        });

        // `DesktopWindow::on_commit` is the operation that refreshes the
        // surface-tree bounding box used by both hit testing and popup
        // placement. The parent compositor already counts commits; avoid an
        // O(surface-tree) walk on every continuously scheduled frame.
        let commit_epoch = self.committed_surfaces;
        let commit_changed = LAST_GEOMETRY_COMMIT.swap(commit_epoch, Ordering::Relaxed) != commit_epoch;
        if commit_changed {
            for binding in &self.xdg_windows {
                if binding.surface.alive() {
                    binding.desktop.on_commit();
                }
            }
        }

        // A client may have missed the first configure while a host resize or
        // teardown was in flight. Retry only the missing handshake; do not
        // resend every configure on every frame.
        let bounds = self.windows.work_area().size;
        if bounds.width >= MIN_HOST_SIZE && bounds.height >= MIN_HOST_SIZE {
            let needs_configure: Vec<_> = self
                .xdg_windows
                .iter()
                .filter_map(|binding| {
                    let (_, initial_configure_sent) = Self::nested_toplevel_protocol_state(&binding.surface)?;
                    if initial_configure_sent || !binding.surface.alive() {
                        return None;
                    }
                    self.windows
                        .window(binding.id)
                        .cloned()
                        .map(|window| (binding.surface.clone(), window))
                })
                .collect();
            for (surface, window) in needs_configure {
                debug!("Retrying an incomplete XDG toplevel configure handshake");
                super::configure_toplevel(&surface, &window, bounds);
            }
        }

        self.sync_nested_space();
        self.sync_nested_keyboard_focus();
    }

    /// Winit focus is separate from Wayland keyboard focus. Losing the host
    /// window should release normal keyboard focus, but never tear down an
    /// active client grab; Smithay will finish that grab when its owner does.
    fn host_focus_changed(&mut self, focused: bool) {
        if focused {
            self.reconcile_nested_clients();
            return;
        }

        // Winit may not deliver the matching button-up when the host loses
        // activation.  Clear shell-owned press state so a later pointer
        // sample cannot keep a widget hold or rest-screen drag alive forever.
        self.pressed_buttons = 0;
        self.widget_hold = None;
        self.last_title_press = None;

        if let Some(keyboard) = self.seat.get_keyboard() {
            if !keyboard.is_grabbed() {
                keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
            }
        }
    }

    fn nested_toplevel_protocol_state(surface: &ToplevelSurface) -> Option<(bool, bool)> {
        compositor::with_states(surface.wl_surface(), |states| {
            let data = states.data_map.get::<XdgToplevelSurfaceData>()?;
            let attributes = data.lock().ok()?;
            Some((attributes.configured, attributes.initial_configure_sent))
        })
    }

    fn nested_surface_has_buffer(surface: &WlSurface) -> bool {
        with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false)
    }

    fn nested_toplevel_parent(surface: &ToplevelSurface) -> Option<WlSurface> {
        compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok()?.parent.clone())
        })
    }

    fn nested_toplevel_ready(surface: &ToplevelSurface) -> bool {
        let Some((configured, initial_configure_sent)) = Self::nested_toplevel_protocol_state(surface) else {
            return false;
        };
        surface.alive()
            && configured
            && initial_configure_sent
            && Self::nested_surface_has_buffer(surface.wl_surface())
    }

    fn nested_window_ready(&self, id: WindowId) -> bool {
        self.xdg_windows
            .iter()
            .find(|binding| binding.id == id)
            .is_some_and(|binding| Self::nested_toplevel_ready(&binding.surface))
    }

    fn nested_window_renderable(&self, id: WindowId) -> bool {
        let Some(window) = self.windows.window(id) else {
            return false;
        };
        self.nested_window_ready(id) && (!window.minimized() || self.animations.contains_key(&id))
    }

    fn nested_window_id_for_surface(&self, surface: &WlSurface) -> Option<WindowId> {
        self.xdg_windows
            .iter()
            .find(|binding| binding.surface.wl_surface() == surface)
            .map(|binding| binding.id)
    }

    /// A transient child is visible only while all of its known toplevel
    /// parents are live, configured and not minimized. A foreign parent is
    /// treated as a root because another compositor-owned object is outside
    /// this nested backend's authority.
    fn nested_transient_visible(&self, id: WindowId) -> bool {
        let mut current = id;
        for _ in 0..=self.xdg_windows.len() {
            let Some(parent_surface) = self
                .xdg_windows
                .iter()
                .find(|binding| binding.id == current)
                .and_then(|binding| Self::nested_toplevel_parent(&binding.surface))
            else {
                return true;
            };

            let Some(parent_id) = self.nested_window_id_for_surface(&parent_surface) else {
                return true;
            };
            if parent_id == current {
                warn!(
                    window_id = id.raw(),
                    "Ignoring a cyclic XDG transient relationship"
                );
                return false;
            }
            let Some(parent_window) = self.windows.window(parent_id) else {
                return false;
            };
            if parent_window.minimized() || !self.nested_window_ready(parent_id) {
                return false;
            }
            current = parent_id;
        }

        warn!(
            window_id = id.raw(),
            "Ignoring an excessively deep XDG transient relationship"
        );
        false
    }

    /// `WindowManager` remains the authority for z-order. This small
    /// dependency pass only makes sure a known transient is placed after its
    /// parent in Smithay's render space; it intentionally does not implement
    /// a second focus or stacking model.
    fn nested_transient_order(&self, mut remaining: Vec<WindowId>) -> Vec<WindowId> {
        let mut ordered = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let next = remaining.iter().position(|id| {
                let Some(parent_surface) = self
                    .xdg_windows
                    .iter()
                    .find(|binding| binding.id == *id)
                    .and_then(|binding| Self::nested_toplevel_parent(&binding.surface))
                else {
                    return true;
                };
                self.nested_window_id_for_surface(&parent_surface)
                    .is_none_or(|parent_id| !remaining.contains(&parent_id))
            });

            let Some(index) = next else {
                warn!("Preserving WindowManager order for a cyclic XDG transient stack");
                ordered.append(&mut remaining);
                break;
            };
            ordered.push(remaining.remove(index));
        }
        ordered
    }

    fn nested_stack(&self, include_minimized_animation: bool) -> Vec<WindowId> {
        let mut stack = Vec::new();
        let workspace = self.windows.current_workspace();
        for window in self.windows.windows() {
            let id = window.id();
            let on_workspace = window.pinned() || window.workspace() == workspace;
            let visible = on_workspace
                && (!window.minimized()
                    || (include_minimized_animation && self.animations.contains_key(&id)));
            if visible && self.nested_window_ready(id) && self.nested_transient_visible(id) {
                stack.push(id);
            }
        }
        self.nested_transient_order(stack)
    }

    fn sync_nested_space(&mut self) {
        let desired_ids = self.nested_stack(true);
        let desired: Vec<(WindowId, SmithayPoint<i32, Logical>)> = desired_ids
            .iter()
            .filter_map(|id| {
                self.windows
                    .window(*id)
                    .map(|window| (*id, (window.frame().origin.x, window.frame().origin.y).into()))
            })
            .collect();

        let current: Vec<(WindowId, SmithayPoint<i32, Logical>)> = self
            .space
            .elements()
            .filter_map(|desktop| {
                let id = self
                    .xdg_windows
                    .iter()
                    .find(|binding| &binding.desktop == desktop)
                    .map(|binding| binding.id)?;
                let location = self.space.element_location(desktop)?;
                Some((id, location))
            })
            .collect();
        let current_ids: Vec<WindowId> = current.iter().map(|(id, _)| *id).collect();
        let desired_active = desired_ids
            .iter()
            .copied()
            .find(|id| self.windows.window(*id).is_some_and(|window| window.active()));
        let current_top = current_ids.last().copied();
        let same_locations = current.len() == desired.len()
            && current.iter().zip(desired.iter()).all(
                |((current_id, current_location), (desired_id, desired_location))| {
                    current_id == desired_id && current_location == desired_location
                },
            );

        // Mapping an element invokes output enter/leave and can be expensive
        // for large surface trees. Only do it when the authoritative stack,
        // geometry or active element really changed.
        if same_locations && desired_active == current_top {
            return;
        }

        let mapped: Vec<_> = self.space.elements().cloned().collect();
        for desktop in mapped {
            self.space.unmap_elem(&desktop);
        }

        for (id, location) in desired {
            let Some(desktop) = self
                .xdg_windows
                .iter()
                .find(|binding| binding.id == id)
                .map(|binding| binding.desktop.clone())
            else {
                continue;
            };
            let active = self.windows.window(id).is_some_and(|window| window.active());
            self.space.map_element(desktop, location, active);
        }
    }

    fn sync_nested_keyboard_focus(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        if keyboard.is_grabbed() {
            return;
        }

        // Compositor-owned surfaces have no Wayland keyboard surface. They
        // therefore temporarily clear client focus while a modal shell is
        // open, then restore the active XDG toplevel when it closes.
        let shell_has_keyboard = self.session != crate::idle::Session::Active
            || self.remove_popup.is_some()
            || self.welcome.is_some()
            || self.control_open
            || self.launcher_open
            || self.finder_open
            || self.settings_open
            || self.app_store_open
            || self.notifications_open
            || (self.setup_open && self.welcome.is_none());
        let desired = if shell_has_keyboard {
            None
        } else {
            self.focused_window()
                .and_then(|id| self.surface_for_window(id))
                .map(|surface| surface.wl_surface().clone())
        };

        if keyboard.current_focus().as_ref() != desired.as_ref() {
            keyboard.set_focus(self, desired, SERIAL_COUNTER.next_serial());
        }
    }

    fn focus_nested_window(&mut self, id: WindowId, serial: smithay::utils::Serial) -> bool {
        if !self.nested_window_ready(id) || self.windows.window(id).is_none_or(|window| window.minimized()) {
            return false;
        }

        if self.windows.focus(id) {
            self.reconfigure_windows();
            self.sync_nested_space();
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            if !keyboard.is_grabbed() {
                let surface = self
                    .surface_for_window(id)
                    .map(|toplevel| toplevel.wl_surface().clone());
                if keyboard.current_focus().as_ref() != surface.as_ref() {
                    keyboard.set_focus(self, surface, serial);
                }
            }
        }
        true
    }

    fn focus_next_window(&mut self, reverse: bool, serial: smithay::utils::Serial) -> bool {
        let ids: Vec<_> = self
            .windows
            .focusable_window_ids()
            .into_iter()
            .filter(|id| self.nested_window_ready(*id) && self.nested_transient_visible(*id))
            .collect();
        if ids.len() < 2 {
            return false;
        }
        let current = self
            .focused_window()
            .and_then(|id| ids.iter().position(|candidate| *candidate == id));
        let Some(index) = next_cycle_index(ids.len(), current, reverse) else {
            return false;
        };
        self.focus_nested_window(ids[index], serial)
    }

    fn nested_popup_state(surface: &PopupSurface) -> Option<NestedPopupState> {
        compositor::with_states(surface.wl_surface(), |states| {
            let data = states.data_map.get::<XdgPopupSurfaceData>()?;
            let attributes = data.lock().ok()?;
            let role_state = (
                attributes.configured,
                attributes.initial_configure_sent,
                attributes.committed,
                attributes.current.geometry,
            );
            drop(attributes);

            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let client_geometry = cached.current().geometry.unwrap_or_default();
            Some(NestedPopupState {
                configured: role_state.0,
                initial_configure_sent: role_state.1,
                committed: role_state.2,
                geometry: role_state.3,
                client_geometry,
            })
        })
    }

    fn nested_popup_parent_state(surface: &WlSurface) -> Option<(WlSurface, SmithayPoint<i32, Logical>)> {
        compositor::with_states(surface, |states| {
            let data = states.data_map.get::<XdgPopupSurfaceData>()?;
            let attributes = data.lock().ok()?;
            Some((attributes.parent.clone()?, attributes.current.geometry.loc))
        })
    }

    fn nested_popup_ready(popup: &PopupSurface) -> bool {
        let Some(state) = Self::nested_popup_state(popup) else {
            return false;
        };
        popup.alive()
            && state.configured
            && state.initial_configure_sent
            && state.committed
            && Self::nested_surface_has_buffer(popup.wl_surface())
    }

    fn nested_popup_root_and_offset(
        &self,
        popup: &PopupSurface,
        own_geometry: SmithayPoint<i32, Logical>,
    ) -> Option<(WlSurface, SmithayPoint<i32, Logical>, usize)> {
        let (mut parent, _) = Self::nested_popup_parent_state(popup.wl_surface())?;
        let mut offset = own_geometry;
        let mut depth = 1;

        // A popup tree should be shallow, but a malformed client must not be
        // allowed to make this traversal loop forever.
        for _ in 0..=self.popups.len() {
            match compositor::get_role(&parent) {
                Some(role) if role == XDG_TOPLEVEL_ROLE => return Some((parent, offset, depth)),
                Some(role) if role == XDG_POPUP_ROLE => {
                    let (next_parent, parent_geometry) = Self::nested_popup_parent_state(&parent)?;
                    offset += parent_geometry;
                    parent = next_parent;
                    depth += 1;
                }
                _ => return None,
            }
        }

        warn!("Ignoring an excessively deep or cyclic XDG popup tree");
        None
    }

    fn nested_popup_placements(&self) -> Vec<NestedPopupPlacement> {
        let roots: Vec<_> = self
            .nested_stack(true)
            .into_iter()
            .filter_map(|id| {
                let binding = self.xdg_windows.iter().find(|binding| binding.id == id)?;
                let location = self.space.element_location(&binding.desktop)?;
                Some((
                    binding.surface.wl_surface().clone(),
                    id,
                    location,
                    binding.desktop.geometry(),
                ))
            })
            .collect();

        let mut placements = Vec::new();
        for (popup_order, popup) in self.popups.iter().enumerate() {
            if !Self::nested_popup_ready(popup) {
                continue;
            }
            let Some(state) = Self::nested_popup_state(popup) else {
                continue;
            };
            let Some((root_surface, relative, depth)) =
                self.nested_popup_root_and_offset(popup, state.geometry.loc)
            else {
                continue;
            };
            let Some((root_order, (_, root_id, root_location, root_geometry))) = roots
                .iter()
                .enumerate()
                .find(|(_, (surface, ..))| *surface == root_surface)
            else {
                // The parent may be a surface owned by another shell. It is
                // not safe to guess its global location in this compositor.
                continue;
            };

            let location = *root_location + root_geometry.loc + relative - state.client_geometry.loc;
            placements.push((
                root_order,
                depth,
                popup_order,
                NestedPopupPlacement {
                    surface: popup.clone(),
                    root: *root_id,
                    location,
                },
            ));
        }

        placements.sort_by_key(|(root_order, depth, popup_order, _)| (*root_order, *depth, *popup_order));
        placements
            .into_iter()
            .map(|(_, _, _, placement)| placement)
            .collect()
    }

    fn nested_popup_under(
        &self,
        position: SmithayPoint<f64, Logical>,
    ) -> Option<(WlSurface, SmithayPoint<f64, Logical>)> {
        for placement in self.nested_popup_placements().into_iter().rev() {
            if let Some((surface, location)) = under_from_surface_tree(
                placement.surface.wl_surface(),
                position,
                placement.location,
                WindowSurfaceType::ALL,
            ) {
                return Some((surface, location.to_f64()));
            }
        }
        None
    }

    fn nested_surface_under(
        &self,
        position: SmithayPoint<f64, Logical>,
    ) -> Option<(WlSurface, SmithayPoint<f64, Logical>)> {
        self.nested_popup_under(position).or_else(|| {
            self.space.element_under(position).and_then(|(window, location)| {
                window
                    .surface_under(position - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, point)| (surface, (point + location).to_f64()))
            })
        })
    }

    /// Translate host input into seat events plus Rouch chrome actions.
    /// Returns whether the frame changed and a redraw is needed.
    fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) -> bool {
        match event {
            InputEvent::Keyboard { event, .. } => {
                // Extract the plain data first: the helpers stay
                // non-generic and the compiler keeps inferring `I`.
                self.process_keyboard(event.key_code(), event.state(), event.time_msec())
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let fallback_size = self.windows.work_area().size;
                let fallback_geometry = smithay::utils::Rectangle::from_size(
                    (
                        fallback_size.width.max(MIN_HOST_SIZE),
                        fallback_size.height.max(MIN_HOST_SIZE),
                    )
                        .into(),
                );
                let output_geometry = self
                    .space
                    .output_geometry(&self.output)
                    .filter(|geometry| geometry.size.w >= MIN_HOST_SIZE && geometry.size.h >= MIN_HOST_SIZE)
                    .unwrap_or(fallback_geometry);
                let position =
                    event.position_transformed(output_geometry.size) + output_geometry.loc.to_f64();
                if !position.x.is_finite() || !position.y.is_finite() {
                    // Never hand malformed device coordinates to Smithay or
                    // geometry hit-testing.  The next valid sample can
                    // recover the pointer without forcing a frame here.
                    return false;
                }
                self.process_motion(position, event.time_msec())
            }
            InputEvent::PointerButton { event, .. } => {
                self.process_button(event.button_code(), event.state(), event.time_msec())
            }
            _ => {
                debug!("Ignoring input event not yet handled by the nested backend");
                false
            }
        }
    }

    /// Feed the native libinput normalizer into the same seat and shell path
    /// used by the nested host backend. The native source transforms absolute
    /// device coordinates to the selected output before this boundary.
    pub(super) fn process_normalized_input_event(&mut self, event: input::NormalizedInputEvent) -> bool {
        use input::{KeyDelivery, NormalizedInputEvent};

        let time_ms = |time_us: u64| -> u32 { (time_us / 1_000).min(u64::from(u32::MAX)) as u32 };

        match event {
            NormalizedInputEvent::Keyboard(event) => {
                let state = match event.state {
                    KeyDelivery::Released => KeyState::Released,
                    KeyDelivery::Pressed | KeyDelivery::Repeat => KeyState::Pressed,
                };
                self.process_keyboard(
                    smithay::input::keyboard::Keycode::from(event.keycode),
                    state,
                    time_ms(event.time_us),
                )
            }
            NormalizedInputEvent::PointerMotion { time_us, dx, dy, .. } => {
                let Some(previous) = self.seat.get_pointer().map(|pointer| pointer.current_location()) else {
                    return false;
                };
                let output_size = self
                    .space
                    .output_geometry(&self.output)
                    .map(|geometry| geometry.size)
                    .unwrap_or_else(|| {
                        let size = self.windows.work_area().size;
                        (size.width, size.height).into()
                    });
                let max_x = output_size.w.saturating_sub(1).max(0) as f64;
                let max_y = output_size.h.saturating_sub(1).max(0) as f64;
                let position = SmithayPoint::new(
                    (previous.x + dx).clamp(0.0, max_x),
                    (previous.y + dy).clamp(0.0, max_y),
                );
                self.note_activity();
                self.process_motion(position, time_ms(time_us))
            }
            NormalizedInputEvent::PointerButton {
                time_us,
                button,
                pressed,
                ..
            } => {
                self.note_activity();
                self.process_button(
                    button,
                    if pressed {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                    time_ms(time_us),
                )
            }
            NormalizedInputEvent::PointerMotionAbsolute { time_us, x, y, .. } => {
                if !x.is_finite() || !y.is_finite() {
                    return false;
                }
                self.note_activity();
                self.process_motion(SmithayPoint::new(x, y), time_ms(time_us))
            }
            NormalizedInputEvent::PointerAxis {
                time_us,
                horizontal,
                vertical,
                horizontal_v120,
                vertical_v120,
                source,
                ..
            } => {
                use smithay::backend::input::{Axis, AxisSource as SmithayAxisSource};
                use smithay::input::pointer::AxisFrame;

                let source = match source {
                    input::AxisSource::Wheel => SmithayAxisSource::Wheel,
                    input::AxisSource::Finger => SmithayAxisSource::Finger,
                    input::AxisSource::Continuous => SmithayAxisSource::Continuous,
                    input::AxisSource::Unknown => SmithayAxisSource::Continuous,
                };
                let mut frame = AxisFrame::new(time_ms(time_us)).source(source);
                if horizontal != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal);
                }
                if vertical != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical);
                }
                if let Some(value) = horizontal_v120 {
                    frame = frame.v120(Axis::Horizontal, value.round() as i32);
                }
                if let Some(value) = vertical_v120 {
                    frame = frame.v120(Axis::Vertical, value.round() as i32);
                }
                if let Some(pointer) = self.seat.get_pointer() {
                    pointer.axis(self, frame);
                    pointer.frame(self);
                }
                false
            }
            NormalizedInputEvent::DeviceRemoved { .. } => {
                self.pressed_buttons = 0;
                self.active_drag = None;
                false
            }
            NormalizedInputEvent::GrabCancelled { .. } => {
                self.pressed_buttons = 0;
                self.active_drag = None;
                false
            }
            NormalizedInputEvent::FocusChanged { active, .. } => {
                if !active {
                    self.pressed_buttons = 0;
                    self.active_drag = None;
                    if let Some(keyboard) = self.seat.get_keyboard() {
                        if !keyboard.is_grabbed() {
                            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
                        }
                    }
                }
                false
            }
            NormalizedInputEvent::DeviceAdded { .. }
            | NormalizedInputEvent::GestureSwipe { .. }
            | NormalizedInputEvent::GesturePinch { .. }
            | NormalizedInputEvent::GestureHold { .. }
            | NormalizedInputEvent::Touch { .. } => false,
        }
    }

    fn process_keyboard(
        &mut self,
        key_code: smithay::input::keyboard::Keycode,
        state: KeyState,
        time: u32,
    ) -> bool {
        let serial = SERIAL_COUNTER.next_serial();
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };

        // Resolve the key once: its keysym, with modifiers.
        let terminal_open = self.terminal_ui.open;
        let shell_overlay_open = self.remove_popup.is_some()
            || self.control_open
            || self.launcher_open
            || self.finder_open
            || self.settings_open
            || self.app_store_open
            || self.notifications_open;
        let mut resolved: Option<(smithay::input::keyboard::Keysym, bool)> = None;
        let mut held_mods = None;
        let _ = keyboard.input::<(), _>(
            self,
            key_code,
            state,
            serial,
            time,
            |_, modifiers, keysym_handle| {
                if state == KeyState::Pressed {
                    resolved = Some((keysym_handle.modified_sym(), modifiers.logo));
                    held_mods = Some(*modifiers);
                }
                let terminal_shortcut = state == KeyState::Pressed
                    && modifiers.logo
                    && keysym_handle.modified_sym().raw() == 0xff0d;
                let shortcut = input::shell_shortcut(
                    keysym_handle.modified_sym().raw(),
                    modifiers.logo,
                    modifiers.ctrl,
                    modifiers.alt,
                    modifiers.shift,
                );
                let shell_shortcut = state == KeyState::Pressed
                    && shortcut.is_some_and(|shortcut| {
                        !matches!(shortcut, input::ShellShortcut::DismissOverlay) || shell_overlay_open
                    });
                if terminal_open || terminal_shortcut || shell_shortcut {
                    FilterResult::Intercept(())
                } else {
                    FilterResult::Forward
                }
            },
        );
        self.note_activity();

        let Some((sym, _logo)) = resolved else {
            // A release: nothing claims those here.
            return false;
        };
        let Some(mods) = held_mods else {
            return false;
        };

        use smithay::input::keyboard::Keysym;
        use smithay::input::keyboard::keysyms;

        let same = |code: u32| sym == Keysym::new(code);

        // The session chain owns keys first: rest and lock consume all.
        match self.session {
            crate::idle::Session::Rest => {
                let advance = if same(keysyms::KEY_space) || same(keysyms::KEY_Return) {
                    crate::idle::RestAdvance::Space
                } else if matches!(sym.raw(), 0x0021..=0x007E) {
                    crate::idle::RestAdvance::Character
                } else {
                    crate::idle::RestAdvance::Other
                };
                return self.session_key(advance);
            }
            crate::idle::Session::Lock => {
                if same(keysyms::KEY_Return) {
                    let _ = self.password_submit();
                    return true;
                }
                if same(keysyms::KEY_BackSpace) {
                    self.password_backspace();
                    return true;
                }
                if same(keysyms::KEY_Escape) {
                    self.session = crate::idle::Session::Rest;
                    return true;
                }
                if let Some(character) = keysym_to_char(sym.raw()) {
                    self.password_type(&character.to_string());
                    return true;
                }
                return true;
            }
            _ => {}
        }

        // The first-run film and update card own input while they are on
        // screen. Setup then owns the keyboard until its required stages are
        // complete, so no client can receive a half-configured session.
        if self.welcome.is_some() {
            return true;
        }
        if self.setup_open {
            if same(keysyms::KEY_Escape)
                && self.setup_state.current_step.is_optional()
                && self.setup_state.status == crate::setup::SetupStatus::InProgress
            {
                return self.setup_skip();
            }
            if same(keysyms::KEY_Return) || same(keysyms::KEY_space) {
                return self.setup_continue();
            }
            return true;
        }

        // The built-in terminal is a shell-owned surface. Its key stream is
        // intercepted before client forwarding so an underlying Wayland app
        // cannot receive terminal input by accident.
        if self.terminal_ui.open {
            return self.process_terminal_keyboard(sym, mods);
        }

        if self.notifications_open && !mods.logo {
            if same(keysyms::KEY_Escape) {
                self.close_notifications();
                return true;
            }
            if same(keysyms::KEY_c) || same(keysyms::KEY_C) {
                self.notifications.clear();
                self.close_notifications();
                return true;
            }
            if same(keysyms::KEY_Return) {
                let now = self.notification_now_ms();
                let groups = self.notifications.grouped_at(now);
                for group in groups {
                    self.notifications.mark_group_read(&group.key, true);
                }
                return true;
            }
            return true;
        }

        if self.app_store_open && !mods.logo {
            if same(keysyms::KEY_Escape) {
                self.app_store_open = false;
                return true;
            }
            if same(keysyms::KEY_Return) {
                return self.app_store_activate(self.app_store.selected.unwrap_or(0));
            }
            if same(keysyms::KEY_BackSpace) {
                self.app_store_backspace();
                return true;
            }
            if let Some(character) = keysym_to_char(sym.raw()) {
                self.app_store_type(&character.to_string());
                return true;
            }
            return true;
        }

        // Shell-owned shortcuts are resolved from one pure table so Alt-Tab,
        // Cmd-Tab, workspace navigation and numbered window focus cannot
        // leak into an application.  The text-field branches above retain
        // ownership of ordinary typing and Backspace.
        if let Some(shortcut) = input::shell_shortcut(sym.raw(), mods.logo, mods.ctrl, mods.alt, mods.shift) {
            return self.handle_shell_shortcut(shortcut, serial);
        }

        // Text fields of open overlays claim printable keys next.
        if self.launcher_open && !mods.logo {
            if same(keysyms::KEY_Escape) {
                self.launcher_open = false;
                return true;
            }
            if same(keysyms::KEY_Return) {
                return self.launcher_activate(0);
            }
            if same(keysyms::KEY_BackSpace) {
                self.launcher_backspace();
                return true;
            }
            if let Some(character) = keysym_to_char(sym.raw()) {
                let text = character.to_string();
                self.launcher_type(&text);
                return true;
            }
            return true;
        }

        if self.finder_open && !mods.logo {
            if same(keysyms::KEY_Escape) {
                self.finder_open = false;
                return true;
            }
            if same(keysyms::KEY_Return) {
                if let Some(index) = self.finder.selection {
                    return self.finder_activate(index);
                }
                return true;
            }
            if same(keysyms::KEY_BackSpace) {
                self.finder_backspace();
                return true;
            }
            if let Some(character) = keysym_to_char(sym.raw()) {
                let text = character.to_string();
                self.finder_type(&text);
                return true;
            }
            return true;
        }

        false
    }

    /// Apply a shell shortcut after modal text fields have had first refusal.
    /// Every action is bounded to compositor-owned state and returns whether
    /// a frame can have changed; an unavailable target still consumes the
    /// shortcut without waking a client or allocating a fallback surface.
    fn handle_shell_shortcut(
        &mut self,
        shortcut: input::ShellShortcut,
        serial: smithay::utils::Serial,
    ) -> bool {
        use input::ShellShortcut;

        match shortcut {
            ShellShortcut::ToggleTerminal => {
                self.toggle_terminal();
                true
            }
            ShellShortcut::ToggleLauncher => {
                self.toggle_launcher();
                true
            }
            ShellShortcut::ToggleGallery => {
                self.toggle_app_store();
                true
            }
            ShellShortcut::ToggleNotifications => {
                self.toggle_notifications();
                true
            }
            ShellShortcut::ToggleFinder => {
                self.toggle_finder();
                true
            }
            ShellShortcut::ToggleSettings => {
                self.toggle_settings();
                true
            }
            ShellShortcut::CycleWindows { reverse } => {
                // A modal shell surface owns navigation while it is open;
                // cycling an underlying client would be surprising and could
                // move focus behind an active dialog.
                if self.remove_popup.is_some()
                    || self.welcome.is_some()
                    || self.setup_open
                    || self.control_open
                    || self.launcher_open
                    || self.finder_open
                    || self.settings_open
                    || self.app_store_open
                    || self.notifications_open
                {
                    return true;
                }
                self.focus_next_window(reverse, serial)
            }
            ShellShortcut::SwitchWorkspace { delta } => {
                if self.windows.switch_workspace_by(delta) {
                    self.reconfigure_windows();
                    self.sync_nested_keyboard_focus();
                }
                true
            }
            ShellShortcut::FocusWindow { number } => self.focus_window_number(number, serial),
            ShellShortcut::CloseWindow => {
                if let Some(id) = self.focused_window() {
                    self.close_toplevel(id);
                }
                true
            }
            ShellShortcut::MinimizeWindow => {
                if let Some(id) = self.focused_window() {
                    if self.windows.window(id).is_some_and(|window| window.minimized()) {
                        self.restore_toplevel(id);
                    } else {
                        self.minimize_toplevel(id);
                    }
                }
                true
            }
            ShellShortcut::ToggleMaximize => {
                if let Some(id) = self.focused_window() {
                    if self.windows.toggle_maximized(id) {
                        self.reconfigure_windows();
                    }
                }
                true
            }
            ShellShortcut::ToggleFullscreen => {
                if let Some(id) = self.focused_window() {
                    if self.windows.toggle_fullscreen(id) {
                        self.reconfigure_windows();
                    }
                }
                true
            }
            ShellShortcut::DismissOverlay => self.dismiss_shell_overlay(),
        }
    }

    /// Focus one of the first nine live windows in MRU order.
    fn focus_window_number(&mut self, number: u8, serial: smithay::utils::Serial) -> bool {
        let ids: Vec<_> = self
            .windows
            .focusable_window_ids()
            .into_iter()
            .filter(|id| self.nested_window_ready(*id) && self.nested_transient_visible(*id))
            .collect();
        let Some(id) = number
            .checked_sub(1)
            .and_then(|index| ids.get(index as usize))
            .copied()
        else {
            return false;
        };
        self.focus_nested_window(id, serial)
    }

    /// Dismiss only the topmost compositor-owned overlay on Escape.
    fn dismiss_shell_overlay(&mut self) -> bool {
        if self.remove_popup.take().is_some() {
            return true;
        }
        if self.control_open {
            self.control_open = false;
            return true;
        }
        if self.notifications_open {
            self.close_notifications();
            return true;
        }
        if self.app_store_open {
            self.app_store_open = false;
            return true;
        }
        if self.launcher_open {
            self.launcher_open = false;
            return true;
        }
        if self.finder_open {
            self.finder_open = false;
            return true;
        }
        if self.settings_open {
            self.settings_open = false;
            return true;
        }
        false
    }

    /// Handle terminal chrome shortcuts and encode basic keyboard input for
    /// the real PTY. The terminal core remains responsible for UTF-8 decoding
    /// and VT parsing; this adapter only sends the bytes a keyboard produced.
    fn process_terminal_keyboard(
        &mut self,
        sym: smithay::input::keyboard::Keysym,
        mods: smithay::input::keyboard::ModifiersState,
    ) -> bool {
        let raw = sym.raw();

        // The explicit global close/open shortcut is predictable and does not
        // get confused with Return sent to the shell.
        if mods.logo && raw == 0xff0d {
            self.toggle_terminal();
            return true;
        }
        if mods.ctrl && mods.shift && (raw == b't' as u32 || raw == b'T' as u32) {
            return self.terminal_new_tab();
        }
        if mods.ctrl && mods.shift && (raw == b'w' as u32 || raw == b'W' as u32) {
            if let Some(index) =
                self.terminal_ui.tabs.iter().position(|tab| {
                    tab.id == self.terminal_ui.active_tab().map(|active| active.id).unwrap_or(0)
                })
            {
                return self.terminal_close_tab(index);
            }
            return true;
        }
        if mods.ctrl && mods.shift && (raw == b'p' as u32 || raw == b'P' as u32) {
            return self.terminal_cycle_theme();
        }
        if mods.ctrl && mods.shift && (raw == b'g' as u32 || raw == b'G' as u32) {
            return self.terminal_toggle_blur();
        }
        if mods.ctrl && mods.shift && (raw == b'a' as u32 || raw == b'A' as u32) {
            return self.terminal_toggle_transparency();
        }

        if self.terminal_ui.title_editing {
            match raw {
                0xff1b => self.terminal_cancel_title_edit(),
                0xff0d => {
                    self.terminal_commit_title();
                }
                0xff08 | 0x007f => self.terminal_title_backspace(),
                _ => {
                    if let Some(character) = keysym_to_char(raw) {
                        self.terminal_title_type(&character.to_string());
                    }
                }
            }
            return true;
        }

        // Clicking the title or Ctrl+Shift+E makes the title editor explicit;
        // regular Return remains a shell byte.
        if mods.ctrl && mods.shift && (raw == b'e' as u32 || raw == b'E' as u32) {
            self.terminal_begin_title_edit();
            return true;
        }

        if mods.logo {
            // Other Super bindings belong to the shell, not the PTY.
            return true;
        }
        if let Some(bytes) = terminal_key_bytes(raw, mods.ctrl, mods.alt, mods.shift) {
            let _ = self.terminal_send_bytes(&bytes);
        }
        true
    }

    /// The window that window shortcuts act on.
    fn focused_window(&self) -> Option<WindowId> {
        self.windows
            .visible_windows()
            .into_iter()
            .rev()
            .find(|window| window.active() && !window.minimized())
            .map(|window| window.id())
    }

    /// Pointer drags on the rest screen advance to the lock screen.
    fn process_drag(&mut self, dx: f64, dy: f64) -> bool {
        if self.session == crate::idle::Session::Rest {
            let _ = dx;
            return self.session_drag(dy as f32);
        }
        false
    }

    fn process_motion(&mut self, position: SmithayPoint<f64, Logical>, time: u32) -> bool {
        let serial = SERIAL_COUNTER.next_serial();
        let previous = self.seat.get_pointer().map(|pointer| pointer.current_location());
        let mut state_changed = false;

        // A held-button drag on the rest screen slides toward the lock.
        if self.pressed_buttons > 0 && self.session == crate::idle::Session::Rest {
            if let Some(previous) = previous {
                let dy = position.y - previous.y;
                if self.process_drag(position.x - previous.x, dy) {
                    debug!("Rest screen dragged upward into the lock screen");
                    state_changed = true;
                }
            }
        }
        let under = if matches!(
            self.session,
            crate::idle::Session::Rest | crate::idle::Session::Lock
        ) || self.launcher_open
            || self.finder_open
            || self.settings_open
            || self.app_store_open
            || self.notifications_open
            || self.terminal_ui.open
            || self.setup_open
            || self.welcome.is_some()
        {
            None
        } else {
            self.nested_surface_under(position)
        };
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.motion(
                self,
                under,
                &MotionEvent {
                    location: position,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
        }

        // Client motion is still forwarded for pointer correctness.  The
        // shell only wakes its renderer for a visible dock hover transition
        // (the dock is the only continuously pointer-shaped shell surface),
        // a dock-band crossing, or an active drag state.  This removes the
        // old full-frame redraw for every client-area mouse pixel.
        state_changed
            || previous.is_some_and(|previous| self.dock_motion_needs_frame(previous, position))
            || previous.is_none()
    }

    /// Decide whether pointer motion can change compositor-owned chrome.
    ///
    /// The generous band covers the magnified dock without allocating its
    /// icon layout for every input event.  The actual dock hit-test remains
    /// authoritative on press; this helper is only a redraw gate.
    fn dock_motion_needs_frame(
        &self,
        previous: SmithayPoint<f64, Logical>,
        next: SmithayPoint<f64, Logical>,
    ) -> bool {
        let Some(previous) = input::VisualPointerPosition::from_f64(previous.x, previous.y) else {
            return true;
        };
        let Some(next) = input::VisualPointerPosition::from_f64(next.x, next.y) else {
            return true;
        };
        if previous == next {
            return false;
        }

        let work_area = self.windows.work_area();
        let dock_band = crate::windowing::Rect::new(
            work_area.origin.x,
            work_area.bottom().saturating_sub(128),
            work_area.size.width.max(0),
            128.min(work_area.size.height.max(0)),
        );
        let previous_point = crate::windowing::Point {
            x: previous.x,
            y: previous.y,
        };
        let next_point = crate::windowing::Point { x: next.x, y: next.y };
        let previous_in_dock_band = dock_band.contains_point(previous_point);
        let next_in_dock_band = dock_band.contains_point(next_point);

        // Entering or leaving the band changes the dock's presence.  While
        // inside it, only horizontal movement changes magnification.
        previous_in_dock_band != next_in_dock_band || (previous_in_dock_band && previous.x != next.x)
    }

    fn process_button(&mut self, button: u32, button_state: ButtonState, time: u32) -> bool {
        let serial = SERIAL_COUNTER.next_serial();
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let primary = input::is_primary_pointer_button(button);

        if button_state == ButtonState::Pressed && primary {
            // Every press is recorded so a following client-initiated
            // xdg move/resize request can be validated against this serial.
            self.pressed_serials.record_press(serial);
            self.pressed_buttons += 1;
        } else if button_state == ButtonState::Released && primary {
            self.pressed_buttons = self.pressed_buttons.saturating_sub(1);
        }

        let mut needs_redraw = false;
        let position = pointer.current_location();
        let terminal_was_open = self.terminal_ui.open;

        // The session chain owns presses first: rest and lock eat everything.
        if matches!(
            self.session,
            crate::idle::Session::Rest | crate::idle::Session::Lock
        ) {
            if button_state == ButtonState::Pressed && primary {
                self.session_press();
                needs_redraw = true;
            }
            return needs_redraw;
        }

        if button_state == ButtonState::Pressed && primary && !pointer.is_grabbed() {
            needs_redraw = self.dispatch_press(position, serial);
        }
        if button_state == ButtonState::Released && primary {
            // Ending a widget hold outside edit mode was a tap; inside edit
            // mode the hold already engaged and the release does nothing.
            let work_area = self.windows.work_area();
            let board_rect = crate::widgets::WidgetBoard::board_rect(work_area);
            let point = crate::windowing::Point {
                x: position.x.round() as i32,
                y: position.y.round() as i32,
            };
            if let Some(index) = self.widget_board.widget_at(board_rect, point) {
                if self.widget_release(index) {
                    needs_redraw = true;
                }
            } else {
                self.widget_hold = None;
            }
        }

        // A shell-owned terminal press must not be replayed into whichever
        // Wayland client happened to retain pointer focus. Capture the state
        // before dispatch as closing the last tab can make `open` false.
        if !terminal_was_open && !self.terminal_ui.open {
            if let Some(pointer) = self.seat.get_pointer() {
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time,
                    },
                );
                pointer.frame(self);
            }
        } else if let Some(pointer) = self.seat.get_pointer() {
            pointer.frame(self);
        }

        needs_redraw
    }

    /// Resolve a press against Rouch chrome first, then against clients.
    fn dispatch_press(
        &mut self,
        position: SmithayPoint<f64, Logical>,
        serial: smithay::utils::Serial,
    ) -> bool {
        let Some(visual_point) = input::VisualPointerPosition::from_f64(position.x, position.y) else {
            return false;
        };
        let point = crate::windowing::Point {
            x: visual_point.x,
            y: visual_point.y,
        };

        // The removal popup is modal: it answers every press itself.
        if self.remove_popup.is_some() {
            return self.popup_press(point);
        }

        // The update card is modal: its dismiss button eats every press
        // until the user continues. Outside clicks only shake the card.
        if matches!(self.welcome, Some(crate::welcome::WelcomeStage::Update)) {
            let frame = crate::welcome::UpdateFrame::elapsed(self.welcome_started.elapsed());
            if frame.finished {
                return self.welcome_dismissed_by(position);
            }
            return true;
        }

        if matches!(self.welcome, Some(crate::welcome::WelcomeStage::FirstRun)) {
            return true;
        }

        // Setup is a true modal surface: an outside click cannot leak to a
        // client while a required installation decision is pending.
        if self.setup_open && self.welcome.is_none() {
            if super::setup_render::button_rect(self.windows.work_area()).contains_point(point) {
                return self.setup_continue();
            }
            return true;
        }

        // Terminal chrome owns its own tab controls and content focus. The
        // close/new actions are handled before client hit testing so they can
        // never leak a click into a window underneath the terminal.
        if self.terminal_ui.open {
            match self.terminal_ui.hit_test(self.windows.work_area(), point) {
                super::terminal_ui::TerminalHit::NewTab => return self.terminal_new_tab(),
                super::terminal_ui::TerminalHit::CloseTab(index) => return self.terminal_close_tab(index),
                super::terminal_ui::TerminalHit::Tab(index) => return self.terminal_select_tab(index),
                super::terminal_ui::TerminalHit::TitleEdit => return self.terminal_begin_title_edit(),
                super::terminal_ui::TerminalHit::Theme => return self.terminal_cycle_theme(),
                super::terminal_ui::TerminalHit::Transparency => return self.terminal_toggle_transparency(),
                super::terminal_ui::TerminalHit::Blur => return self.terminal_toggle_blur(),
                super::terminal_ui::TerminalHit::Content => {
                    self.terminal_ui.focus();
                    return true;
                }
                super::terminal_ui::TerminalHit::Title | super::terminal_ui::TerminalHit::Window => {
                    self.terminal_ui.focus();
                    return true;
                }
                super::terminal_ui::TerminalHit::None => {}
            }
        }

        // The top bar answers first, like the macOS menu bar.
        let bar_hit = crate::topbar::item_at(&self.topbar_layout, point);
        if bar_hit != crate::topbar::TopBarItem::Background && self.press_topbar(bar_hit) {
            self.control_layout = crate::control::layout(self.windows.work_area());
            return true;
        }

        // The open control centre swallows the next press: toggles inside
        // apply, presses outside close the panel.
        if self.control_open {
            if let Some(control) = crate::control::control_at(&self.control_layout, point) {
                let value = match control {
                    crate::control::Control::Brightness => Some(crate::control::slider_value(
                        self.control_layout.brightness,
                        point,
                    )),
                    crate::control::Control::Volume => {
                        Some(crate::control::slider_value(self.control_layout.volume, point))
                    }
                    _ => None,
                };
                self.apply_control(control, value);
                return true;
            }
            self.control_open = false;
            return true;
        }

        // The open notification centre owns clicks inside its grouped cards;
        // clicking outside closes it without dismissing any queued alert.
        if self.notifications_open {
            let now = self.notification_now_ms();
            let groups = self.notifications.grouped_at(now);
            let panel = super::notification_render::panel_rect(self.windows.work_area(), true, groups.len());
            if panel.contains_point(point) {
                for (index, group) in groups.iter().enumerate() {
                    if super::notification_render::group_rect(panel, index, true).contains_point(point) {
                        self.notifications.mark_group_read(&group.key, true);
                        return true;
                    }
                }
                return true;
            }
            self.close_notifications();
            return true;
        }

        // The gallery uses the same two-step selection/activation interaction
        // as a native store: first click selects a card, second click invokes
        // its explicit Install, Update, or Open action.
        if self.app_store_open {
            let work_area = self.windows.work_area();
            match self.app_store.hit_test(work_area, point) {
                crate::app_store::AppStoreHit::Category(index) => {
                    self.app_store.set_category_index(index);
                    return true;
                }
                crate::app_store::AppStoreHit::Card(index) => {
                    return self.app_store_activate(index);
                }
                crate::app_store::AppStoreHit::Search => return true,
                crate::app_store::AppStoreHit::None => {
                    self.app_store_open = false;
                    return true;
                }
            }
        }

        // The launcher sheet: tiles launch, outside presses dismiss.
        if self.launcher_open {
            let work_area = self.windows.work_area();
            let sheet = crate::launcher::sheet_rect(work_area);
            let hits = crate::launcher::filter_apps(&self.applications, &self.launcher_query);
            if let Some(index) = crate::launcher::tile_at(sheet, hits.len(), point) {
                return self.launcher_activate(index);
            }
            self.launcher_open = false;
            return true;
        }

        // The Finder window: sidebar navigates, back button, grid opens.
        if self.finder_open {
            let work_area = self.windows.work_area();
            let window = crate::finder::window_rect(work_area);
            let (sidebar, toolbar, _content, grid) = crate::finder::layout(window);

            if let Some(button) = crate::finder::toolbar_button_at(window, toolbar, point) {
                if button == 0 {
                    self.finder_back();
                } else {
                    self.finder_forward();
                }
                return true;
            }
            if let Some(place) = crate::finder::sidebar_hit(sidebar, point) {
                let places = crate::finder::sidebar_places();
                if let Some((_, path)) = places.get(place) {
                    let target = path.clone();
                    self.finder_navigate(&target);
                    return true;
                }
            }
            let visible = self.finder.filtered(&self.finder_items);
            if let Some(index) = crate::finder::item_at(grid, visible.len(), point) {
                if self.finder.selection == Some(index) {
                    self.finder_activate(index);
                } else {
                    self.finder.selection = Some(index);
                }
                return true;
            }
            return true;
        }

        // The Settings window: sidebar switches panes, rows apply values.
        if self.settings_open {
            let work_area = self.windows.work_area();
            let rows = self.settings_backend.pane_rows(
                self.settings_pane,
                &crate::user::UserProfile {
                    name: self.profile_name(),
                    avatar: crate::user::AvatarSource::default(),
                },
                &self.toggles,
            );
            let layout = crate::settings::layout(work_area, self.settings_pane, rows.len());

            if let Some(pane) = crate::settings::sidebar_hit(&layout, point) {
                self.settings_pane(pane);
                return true;
            }
            if let Some(index) = crate::settings::row_hit(&layout, point) {
                if let Some(setting) = rows.get(index) {
                    match setting {
                        crate::settings::Setting::Toggle { key, value, .. } => {
                            let next = !value;
                            self.settings_apply(key, crate::settings::SettingValue::Bool(next));
                        }
                        crate::settings::Setting::Select {
                            key,
                            options,
                            selected,
                            ..
                        } => {
                            let next = (*selected + 1) % options.len().max(1);
                            self.settings_apply(key, crate::settings::SettingValue::Choice(next));
                        }
                        crate::settings::Setting::Slider {
                            key, value, min, max, ..
                        } => {
                            // A row press on a slider jumps to the point's
                            // fraction of the track, like macOS.
                            let row = layout.rows[index];
                            let fraction = (point.x - row.origin.x - 150) as f32 / 130.0;
                            let span = (max - min).abs().max(1e-6);
                            let level = min + span * fraction.clamp(0.0, 1.0);
                            let _ = value;
                            self.settings_apply(key, crate::settings::SettingValue::Float(level));
                        }
                        crate::settings::Setting::Action { key, .. } => {
                            self.settings_apply(key, crate::settings::SettingValue::Bool(true));
                        }
                        _ => {}
                    }
                }
                return true;
            }
            return true;
        }

        // Desktop icons, below the bar, right of the widget rail.
        {
            let work_area = self.windows.work_area();
            if let Some(index) = self.desktop.icon_at(work_area, point) {
                self.desktop.selected = Some(index);
                return self.desktop_activate(index);
            }
        }

        // The dock sits above windows: resolve a dock hit next.
        {
            let work_area = self.windows.work_area();
            if let Some(index) = self.dock.hit(work_area, point) {
                self.activate_dock_item(index);
                return true;
            }
        }

        // Widgets: a press starts a hold; the release either tapped the
        // minus button (edit mode) or entered edit mode after the threshold.
        {
            let work_area = self.windows.work_area();
            let board_rect = crate::widgets::WidgetBoard::board_rect(work_area);
            if let Some(index) = self.widget_board.widget_at(board_rect, point) {
                if self.widget_board.editing {
                    if self.widget_board.minus_hit(board_rect, index, point) {
                        self.open_remove_popup(index);
                        return true;
                    }
                    // A press on any widget while editing ends edit mode.
                    self.widget_board.set_editing(false);
                    return true;
                }
                let now = Instant::now();
                let held = self.widget_hold_tick(index, now);
                let _ = held;
                return true;
            }
        }

        // Topmost first: the active window is last in z-order.
        let hit = self
            .windows
            .visible_windows()
            .into_iter()
            .rev()
            .find(|window| !window.minimized() && window.frame().contains_point(point))
            .map(|window| (window.id(), action_at(window, point)));

        let Some((id, action)) = hit else {
            // Click on bare desktop: drop focus like macOS does.
            self.windows.clear_focus();
            self.reconfigure_windows();
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, Option::<WlSurface>::None, serial);
            }
            return true;
        };

        // Focusing happens for every chrome and client press inside a window;
        // the action then decides what else occurs.
        if self.windows.focus(id) {
            self.reconfigure_windows();
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            let surface = self
                .surface_for_window(id)
                .map(|toplevel| toplevel.wl_surface().clone());
            keyboard.set_focus(self, surface, serial);
        }

        match action {
            ChromeAction::TrafficLight(kind) => {
                self.begin_chrome_action(
                    id,
                    ChromeAction::TrafficLight(kind),
                    (position.x, position.y),
                    serial,
                );
                true
            }
            ChromeAction::DragTitleBar => self.handle_title_bar_press(id, position, serial),
            ChromeAction::Resize(edge) => {
                self.begin_chrome_action(id, ChromeAction::Resize(edge), (position.x, position.y), serial);
                true
            }
            ChromeAction::Client => false,
        }
    }

    /// Title-bar press: double-click toggles maximize, single press drags.
    fn handle_title_bar_press(
        &mut self,
        id: WindowId,
        position: SmithayPoint<f64, Logical>,
        serial: smithay::utils::Serial,
    ) -> bool {
        let now = Instant::now();
        let (is_double, same_spot) = match self.last_title_press {
            Some((at, spot)) => (
                now.duration_since(at).as_millis() <= DOUBLE_CLICK_MS,
                (spot.x - position.x).abs() < 3.0 && (spot.y - position.y).abs() < 3.0,
            ),
            None => (false, false),
        };

        if is_double && same_spot {
            self.last_title_press = None;
            self.windows.toggle_maximized(id);
            self.reconfigure_windows();
            return true;
        }

        self.last_title_press = Some((now, position));
        self.begin_chrome_action(id, ChromeAction::DragTitleBar, (position.x, position.y), serial);
        true
    }
}

/// Paint the launcher sheet into a one-shot element for this frame.
///
/// The sheet reuses the widget board's renderer surface? No: a local buffer
/// keeps the launcher independent of the board's damage state.
fn paint_launcher<R>(
    renderer: &mut R,
    work_area: crate::windowing::Rect,
    apps: &[crate::desktop_entry::DesktopEntry],
    query: &str,
    fonts: &super::fonts::FontBook,
) -> Option<smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<R>>
where
    R: smithay::backend::renderer::Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    use smithay::backend::renderer::element::{Kind, memory::MemoryRenderBuffer};
    use smithay::utils::Point as PhysPoint;

    let sheet = crate::launcher::sheet_rect(work_area);
    let width = sheet.size.width.max(1);
    let height = sheet.size.height.max(1);

    let mut buffer = MemoryRenderBuffer::new(
        smithay::backend::allocator::Fourcc::Argb8888,
        (width, height),
        1,
        smithay::utils::Transform::Normal,
        None,
    );
    {
        let mut context = buffer.render();
        let w = width as usize;
        let _ = context.draw(|pixels| {
            for px in pixels.chunks_exact_mut(4) {
                px.copy_from_slice(&[0, 0, 0, 0]);
            }

            super::pixel::fill_round_rect(pixels, w, sheet_local(sheet), 24, [240, 246, 252, 235]);
            let hits = crate::launcher::filter_apps(apps, query);
            for (index, app) in hits.iter().enumerate() {
                let tile = crate::launcher::tile_rect(sheet, index);
                let local = Rect {
                    origin: Point {
                        x: tile.origin.x - sheet.origin.x,
                        y: tile.origin.y - sheet.origin.y,
                    },
                    size: tile.size,
                };
                super::pixel::fill_round_rect(pixels, w, local, 16, app_tint(&app.id));
                let letter: String = app
                    .name
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_ascii_uppercase()
                    .to_string();
                let text_w = fonts.text_width(&letter, 26.0);
                let label_rect = Rect {
                    origin: Point {
                        x: local.origin.x + (local.size.width - text_w) / 2,
                        y: local.origin.y + 14,
                    },
                    size: crate::windowing::Size::new(text_w + 4, 30),
                };
                fonts.draw_text(pixels, w, label_rect, [255, 255, 255, 250], &letter, 26.0);
                let name_w = fonts.text_width(&app.name, 9.0);
                let name_rect = Rect {
                    origin: Point {
                        x: local.origin.x + (local.size.width - name_w) / 2,
                        y: local.origin.y + local.size.height - 16,
                    },
                    size: crate::windowing::Size::new(name_w + 4, 12),
                };
                fonts.draw_text(pixels, w, name_rect, [40, 44, 52, 255], &app.name, 9.0);
            }

            let search = crate::launcher::search_rect(sheet);
            let local = Rect {
                origin: Point {
                    x: search.origin.x - sheet.origin.x,
                    y: search.origin.y - sheet.origin.y,
                },
                size: search.size,
            };
            super::pixel::fill_round_rect(pixels, w, local, 10, [216, 222, 228, 255]);
            let shown = if query.is_empty() { "Search apps" } else { query };
            let text_w = fonts.text_width(shown, 9.0);
            fonts.draw_text(
                pixels,
                w,
                Rect {
                    origin: Point {
                        x: local.origin.x + 10,
                        y: local.origin.y + 9,
                    },
                    size: crate::windowing::Size::new(text_w + 4, 12),
                },
                if query.is_empty() {
                    [120, 128, 138, 255]
                } else {
                    [30, 36, 44, 255]
                },
                shown,
                9.0,
            );
            Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                smithay::utils::Size::from((width, height)),
            )])
        });
    }

    smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        PhysPoint::from((sheet.origin.x as f64, sheet.origin.y as f64)),
        &buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )
    .ok()
}

/// The sheet rect in the sheet-local buffer coordinates.
fn sheet_local(sheet: Rect) -> Rect {
    Rect::new(0, 0, sheet.size.width, sheet.size.height)
}

/// A stable colour for one app id, from the ocean palette.
fn app_tint(app_id: &str) -> [u8; 4] {
    let hash = app_id
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    const PALETTE: [[u8; 4]; 6] = [
        [12, 102, 160, 255],
        [22, 142, 175, 255],
        [72, 165, 190, 255],
        [36, 84, 141, 255],
        [94, 148, 178, 255],
        [23, 118, 141, 255],
    ];
    PALETTE[(hash % PALETTE.len() as u32) as usize]
}

/// Paint the rest screen: the wallpaper dimmed with a gentle hint.
fn paint_rest_screen<R>(
    renderer: &mut R,
    work_area: crate::windowing::Rect,
    fonts: &super::fonts::FontBook,
) -> Option<smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<R>>
where
    R: smithay::backend::renderer::Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    use smithay::backend::renderer::element::{Kind, memory::MemoryRenderBuffer};
    use smithay::utils::Point as PhysPoint;

    let width = work_area.size.width.max(1);
    let height = work_area.size.height.max(1);
    let mut buffer = MemoryRenderBuffer::new(
        smithay::backend::allocator::Fourcc::Argb8888,
        (width, height),
        1,
        smithay::utils::Transform::Normal,
        None,
    );
    {
        let mut context = buffer.render();
        let w = width as usize;
        let _ = context.draw(|pixels| {
            for px in pixels.chunks_exact_mut(4) {
                px.copy_from_slice(&[10, 12, 18, 178]);
            }
            let hint = "Press Space or drag up to unlock";
            let hint_w = fonts.text_width(hint, 12.0);
            let rect = Rect::new((width - hint_w) / 2, height - 120, hint_w + 4, 20);
            fonts.draw_text(pixels, w, rect, [200, 208, 220, 230], hint, 12.0);
            Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                smithay::utils::Size::from((width, height)),
            )])
        });
    }

    smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        PhysPoint::from((0.0, 0.0)),
        &buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )
    .ok()
}

/// Paint the lock screen: avatar, name, password dots and a hint.
fn paint_lock_screen<R>(
    renderer: &mut R,
    work_area: crate::windowing::Rect,
    name: &str,
    password: &crate::idle::PasswordEntry,
    fonts: &super::fonts::FontBook,
) -> Option<smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<R>>
where
    R: smithay::backend::renderer::Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    use smithay::backend::renderer::element::{Kind, memory::MemoryRenderBuffer};
    use smithay::utils::Point as PhysPoint;

    let width = work_area.size.width.max(1);
    let height = work_area.size.height.max(1);
    let mut buffer = MemoryRenderBuffer::new(
        smithay::backend::allocator::Fourcc::Argb8888,
        (width, height),
        1,
        smithay::utils::Transform::Normal,
        None,
    );
    {
        let mut context = buffer.render();
        let w = width as usize;
        let _ = context.draw(|pixels| {
            for px in pixels.chunks_exact_mut(4) {
                px.copy_from_slice(&[6, 10, 22, 245]);
            }

            let (avatar, name_rect, field) = crate::idle::lock_layout(work_area);
            let (outer, inner) = crate::user::AvatarMark::Ocean.palette();
            let cx = avatar.origin.x + avatar.size.width / 2;
            let cy = avatar.origin.y + avatar.size.height / 2;
            super::pixel::fill_circle(pixels, w, cx, cy, avatar.size.width / 2, outer);
            super::pixel::fill_circle(pixels, w, cx, cy, avatar.size.width / 3, inner);

            let name_w = fonts.text_width(name, 14.0);
            fonts.draw_text(
                pixels,
                w,
                Rect::new(
                    name_rect.origin.x + (name_rect.size.width - name_w) / 2,
                    name_rect.origin.y,
                    name_w + 4,
                    20,
                ),
                [238, 242, 250, 255],
                name,
                14.0,
            );

            // Password field with one dot per typed character.
            super::pixel::fill_round_rect(pixels, w, field, 10, [216, 222, 232, 255]);
            let dots = password.buffer.chars().count().min(18);
            for i in 0..dots {
                let dot_x = field.origin.x + 18 + i as i32 * 16;
                super::pixel::fill_circle(
                    pixels,
                    w,
                    dot_x,
                    field.origin.y + field.size.height / 2,
                    5,
                    [30, 38, 52, 255],
                );
            }
            let hint = "Enter password and press Return";
            let hint_w = fonts.text_width(hint, 8.0);
            fonts.draw_text(
                pixels,
                w,
                Rect::new((width - hint_w) / 2, field.bottom() + 14, hint_w + 4, 12),
                [150, 160, 178, 220],
                hint,
                8.0,
            );
            Result::<_, ()>::Ok(vec![smithay::utils::Rectangle::from_size(
                smithay::utils::Size::from((width, height)),
            )])
        });
    }

    smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        PhysPoint::from((0.0, 0.0)),
        &buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )
    .ok()
}

/// Encode the small set of non-printable keys needed by an interactive shell.
/// UTF-8 text itself is sent from the keysym character below; the terminal
/// core then performs the mandatory stream decoding and VT parsing.
fn terminal_key_bytes(raw: u32, ctrl: bool, alt: bool, shift: bool) -> Option<Vec<u8>> {
    if ctrl && ((b'a' as u32..=b'z' as u32).contains(&raw) || (b'A' as u32..=b'Z' as u32).contains(&raw)) {
        return Some(vec![(raw as u8).to_ascii_lowercase() & 0x1f]);
    }

    let mut bytes = match raw {
        0xff0d => vec![b'\r'],
        0xff08 | 0x007f => vec![0x7f],
        0xff1b => vec![0x1b],
        0xff09 if shift => b"\x1b[Z".to_vec(),
        0xff09 => vec![b'\t'],
        0xff51 => b"\x1b[D".to_vec(),
        0xff52 => b"\x1b[A".to_vec(),
        0xff53 => b"\x1b[C".to_vec(),
        0xff54 => b"\x1b[B".to_vec(),
        0xff50 => b"\x1b[H".to_vec(),
        0xff57 => b"\x1b[F".to_vec(),
        0xffff => b"\x1b[3~".to_vec(),
        _ => {
            let character = keysym_to_char(raw)?;
            character.to_string().into_bytes()
        }
    };
    if alt && !bytes.starts_with(&[0x1b]) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

/// Map a Latin-1 keysym to its character, for text fields and basic terminal
/// input. More complex compose/IME input remains owned by the native seat
/// keymap and will be added when text-input-v3 is connected.
fn keysym_to_char(raw: u32) -> Option<char> {
    match raw {
        0x0020..=0x007E => char::from_u32(raw),
        0x00A0..=0x00FF => char::from_u32(raw),
        _ => None,
    }
}
