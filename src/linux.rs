//! Linux Wayland server and XDG window-management layer.
//!
//! The nested and native backends share the same protocol-correct lifecycle of
//! XDG toplevels/popups and the same scene builder. The pure `windowing` module
//! owns the desktop rules shared by pointer, shortcut, dock and title-bar
//! actions.

use std::{ffi::OsString, sync::Arc, time::Instant};

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::utils::on_commit_buffer_handler},
    delegate_alpha_modifier, delegate_compositor, delegate_data_device, delegate_dmabuf,
    delegate_output, delegate_primary_selection, delegate_seat, delegate_shm,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{Space, Window as DesktopWindow},
    input::{Seat, SeatHandler, SeatState},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{EventLoop, Interest, Mode as LoopMode, PostAction, generic::Generic},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Physical, Serial, Size, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::{OutputHandler, OutputManagerState},
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                set_data_device_focus,
            },
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState, set_primary_focus},
        },
        shell::xdg::{
            Configure, PopupSurface, PositionerState, ShellClient, ToplevelSurface, XdgShellHandler,
            XdgShellState,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};
use tracing::{debug, info, trace, warn};

use crate::{
    app_store_backend,
    chrome::{ChromeAction, TrafficLightKind},
    design::{GlassRecipe, GlassSurface, PerformanceProfile, VisualProfile, glass_recipe},
    windowing::{
        Point as RouchPoint, Rect, ResizeEdge, Size as WindowSize, SizeLimits, Window, WindowId,
        WindowManager,
    },
};

mod app_store_render;
mod backends;
mod compat;
mod decorations;
mod dock_render;
mod drm;
mod finder_backend;
mod finder_render;
mod fonts;
mod game_mode_backend;
mod grabs;
mod input;
mod nested;
mod notification_render;
mod outputs;
mod pixel;
mod render_policy;
mod settings_backend;
mod settings_render;
mod setup_render;
mod shell_render;
mod terminal_render;
mod terminal_ui;
mod wallpaper;
mod welcome_render;
mod widget_render;

use decorations::Decorations;
use dock_render::DockRenderer;
use grabs::{DragTarget, PressedSerials};
use welcome_render::WelcomeRenderer;

/// The desktop name applications and portals see.
///
/// It must match the `DesktopNames` of the installed Wayland session entry and
/// the portal configuration the installer writes, or `xdg-desktop-portal` finds
/// no backend and every file dialog, screenshot and screen share fails.
pub(crate) const CURRENT_DESKTOP: &str = "Caelune";

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let options = crate::session::parse_args(args)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let environment = crate::session::SessionEnvironment::from_process();

    tracing::info!(
        mode = %options.mode,
        fallback_nested = options.fallback_nested,
        readiness = %environment.native_readiness(),
        xdg = %environment.diagnostic(),
        log = ?session_log_path(),
        "Rouch startup policy"
    );
    let compatibility = compat::CapabilityMatrix::detect();
    let unavailable_capabilities = compatibility
        .iter()
        .filter(|status| status.state.is_unavailable())
        .count();
    tracing::info!(
        input_backend = ?input::select_input_backend(cfg!(feature = "native-session"), true),
        known_outputs = outputs::OutputRegistry::new().snapshots().count(),
        unavailable_capabilities,
        "Rouch Tahoe compatibility snapshot"
    );

    match crate::session::startup_plan(&options) {
        crate::session::StartupPlan::RunNested => nested::run(),
        crate::session::StartupPlan::TryNative { fallback_nested } => {
            record_session_event(&format!(
                "native start: readiness={}, {}",
                environment.native_readiness(),
                environment.diagnostic()
            ));
            match drm::run_native(&options, &environment) {
                Ok(()) => {
                    record_session_event("native session ended normally");
                    Ok(())
                }
                Err(error) if fallback_nested => {
                    record_session_event(&format!(
                        "native session unavailable ({error}); falling back to nested"
                    ));
                    tracing::warn!(
                        kind = %error.kind,
                        detail = %error.detail,
                        "Native session unavailable; releasing native resources and falling back to nested"
                    );
                    nested::run()
                }
                Err(error) => {
                    record_session_event(&format!("native session failed: {error}"));
                    Err(Box::new(error))
                }
            }
        }
    }
}

/// Where a native session records why it started and why it stopped.
///
/// A display manager restarts its greeter the moment the compositor exits, and
/// it truncates its own session log on every attempt, so stderr is gone before
/// anyone can read it. This file appends and is the only trace that survives a
/// failed login.
fn session_log_path() -> std::path::PathBuf {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".local")
                .join("state")
        });
    state_home.join("rouch").join("session.log")
}

/// Append one timestamped line to the session log. Never fails the startup:
/// an unwritable state directory must not be the reason a session does not run.
fn record_session_event(message: &str) {
    use std::io::Write as _;

    let path = session_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "[{seconds}] {message}"));
    if let Err(error) = written {
        warn!(path = ?path, ?error, "Could not append to the Rouch session log");
    }
}

/// Where Rouch remembers the last-run version across sessions.
fn state_path() -> std::path::PathBuf {
    let home = std::env::var_os("XDG_STATE_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".local").join("state").join("rouch").join("version")
}

/// The resumable setup state lives beside the remembered release version.
fn setup_state_path() -> std::path::PathBuf {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        return std::path::PathBuf::from(state_home)
            .join("rouch")
            .join(crate::setup::SETUP_STATE_FILENAME);
    }
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    crate::setup::default_state_path(&home)
}

/// Load setup without making startup fail when an older/corrupt state file is
/// found. The first-run card will simply offer a fresh resumable flow.
fn load_setup_state() -> crate::setup::SetupState {
    std::fs::read_to_string(setup_state_path())
        .ok()
        .and_then(|contents| crate::setup::SetupState::decode(&contents).ok())
        .unwrap_or_default()
}

/// Read one boolean preference without creating a config directory at boot.
fn read_config_bool(key: &str, default: bool) -> bool {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match std::fs::read_to_string(home.join(".config").join("rouch").join(key))
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("1" | "true" | "on" | "yes") => true,
        Some("0" | "false" | "off" | "no") => false,
        _ => default,
    }
}

/// Restore notification delivery preferences before the first client maps.
fn notification_center_from_config() -> crate::notifications::NotificationCenter {
    let mut preferences = crate::notifications::NotificationPreferences::default();
    preferences.set_enabled(read_config_bool("notifications.enabled", true));
    preferences.set_do_not_disturb(read_config_bool("notifications.dnd", false));
    crate::notifications::NotificationCenter::with_preferences(preferences)
}

/// The running user's account name, resolved once.
fn whoami_cached() -> String {
    static USER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    USER.get_or_init(crate::user::whoami).clone()
}

/// Locations of `unix_chkpwd`, the setuid helper `pam_unix` itself uses.
const UNIX_CHKPWD_PATHS: [&str; 4] = [
    "/usr/lib/unix_chkpwd",
    "/usr/sbin/unix_chkpwd",
    "/sbin/unix_chkpwd",
    "/usr/libexec/unix_chkpwd",
];

/// Verify a password for `user`, the way the system itself does.
///
/// The compositor runs as an unprivileged user, so it cannot read
/// `/etc/shadow`: every direct comparison against it fails and the lock screen
/// can never be unlocked. `unix_chkpwd` is the setuid helper `pam_unix`
/// delegates to for exactly this reason; it reads the password from stdin and
/// exits zero when it matches. The `/etc/shadow` path below stays as a
/// fallback for a session that really does run privileged.
fn verify_password(user: &str, typed: &str) -> bool {
    if typed.is_empty() {
        return false;
    }
    if let Some(accepted) = verify_password_with_helper(user, typed) {
        return accepted;
    }
    let Some(shadow_line) = shadow_hash_for(user) else {
        warn!("No password backend is reachable; the lock screen cannot verify this account");
        return false;
    };
    let Some((_, salt, _)) = split_crypt_fields(&shadow_line) else {
        return false;
    };

    let output = std::process::Command::new("openssl")
        .args(["passwd", "-6", "-salt", &salt, "--", typed])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let computed = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            constant_time_eq(&computed, &shadow_line)
        }
        _ => false,
    }
}

/// Ask `unix_chkpwd` to check the password.
///
/// `None` means no helper was usable, so the caller should try its fallback;
/// `Some(false)` is a real rejection.
fn verify_password_with_helper(user: &str, typed: &str) -> Option<bool> {
    use std::io::Write as _;

    let helper = UNIX_CHKPWD_PATHS
        .iter()
        .find(|path| std::path::Path::new(path).exists())?;

    let mut child = std::process::Command::new(helper)
        .arg(user)
        .arg("nullok")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| warn!(helper, ?error, "Could not run the password helper"))
        .ok()?;

    // The helper expects the password on stdin, NUL-terminated and without a
    // trailing newline.
    let mut secret = typed.as_bytes().to_vec();
    secret.push(0);
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(&secret);
    }
    // Dropping stdin closes the pipe, which the helper waits for.
    drop(child.stdin.take());

    let status = child
        .wait()
        .map_err(|error| warn!(helper, ?error, "Password helper did not exit cleanly"))
        .ok()?;
    Some(status.success())
}

/// The account's hash field from /etc/shadow.
fn shadow_hash_for(user: &str) -> Option<String> {
    let shadow = std::fs::read_to_string("/etc/shadow").ok()?;
    shadow
        .lines()
        .find(|line| line.starts_with(user))
        .and_then(|line| line.split(':').nth(1))
        .filter(|hash| !hash.is_empty() && !hash.starts_with('!') && !hash.starts_with('*'))
        .map(str::to_owned)
}

/// Split a `$6$salt$hash` field into (id, salt, hash).
fn split_crypt_fields(hash: &str) -> Option<(String, String, String)> {
    let mut parts = hash.split('$');
    let _empty = parts.next()?;
    let id = parts.next()?.to_owned();
    let salt = parts.next()?.to_owned();
    let digest = parts.next()?.to_owned();
    Some((id, salt, digest))
}

/// A constant-time string comparison, so the lock screen never leaks how
/// much of the password matched.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Scan the freedesktop application directories once.
/// Every directory that can hold a `.desktop` entry, in XDG precedence order.
///
/// Flatpak exports its entries under `exports/share/applications` rather than
/// the plain data directory. The application gallery installs with
/// `flatpak run --user`, so without the user export path nothing it installs
/// ever reaches the launcher or the dock.
fn application_directories() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;

    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"));
        dirs.push(data_home.join("applications"));
        dirs.push(data_home.join("flatpak/exports/share/applications"));
    }

    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let bases: Vec<PathBuf> = if data_dirs.trim().is_empty() {
        vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]
    } else {
        data_dirs
            .split(':')
            .filter(|entry| !entry.is_empty())
            .map(PathBuf::from)
            .collect()
    };
    for base in bases {
        dirs.push(base.join("applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs.push(PathBuf::from("/var/lib/snapd/desktop/applications"));

    dirs.dedup();
    dirs
}

fn scan_applications() -> Vec<crate::desktop_entry::DesktopEntry> {
    let dirs = application_directories();
    // The first directory that defines an id wins, matching XDG precedence:
    // a user override must shadow the system entry, not duplicate it.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut apps = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("desktop"))
            {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let id = path.file_stem().map(|stem| stem.to_string_lossy().into_owned());
            let Some(id) = id else { continue };
            if !seen.insert(id.clone()) {
                continue;
            }
            let entry = crate::desktop_entry::DesktopEntry::parse(&id, &body);
            if entry.visible() {
                apps.push(entry);
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Map the user's Desktop directory and pinned apps to desktop icons.
fn scan_desktop() -> crate::desktop_icons::Desktop {
    let dir = crate::user::desktop_dir();
    let mut listing: Vec<(String, bool)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let directory = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            listing.push((name, directory));
        }
    }
    let shortcuts: Vec<crate::desktop_entry::DesktopEntry> = Vec::new();
    crate::desktop_icons::Desktop::from_listing(&listing, &shortcuts)
}

/// Global state for one Rouch desktop session.
pub struct Rouch {
    started_at: Instant,
    socket_name: OsString,
    display_handle: DisplayHandle,

    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    _output_manager_state: OutputManagerState,
    _alpha_modifier_state: smithay::wayland::alpha_modifier::AlphaModifierState,
    /// Clipboard and drag-and-drop. Without this global no application can
    /// copy or paste, which rules the desktop out for daily use.
    data_device_state: DataDeviceState,
    /// Middle-click paste, which X11 users expect from every toolkit.
    primary_selection_state: PrimarySelectionState,
    /// Server-side decoration negotiation. Rouch draws its own chrome, so a
    /// client that also draws CSD would stack two title bars.
    _xdg_decoration_state: XdgDecorationState,
    /// GPU buffer sharing. Mesa's EGL Wayland platform refuses to initialize
    /// without it, so every accelerated client falls back to software or fails
    /// outright until the global exists.
    dmabuf_state: DmabufState,
    dmabuf_global: Option<DmabufGlobal>,
    seat_state: SeatState<Rouch>,
    seat: Seat<Rouch>,

    windows: WindowManager,
    xdg_windows: Vec<XdgWindow>,
    popups: Vec<PopupSurface>,
    output: Output,
    space: Space<DesktopWindow>,
    loop_signal: smithay::reexports::calloop::LoopSignal,
    /// Dirty bit consumed by the nested compositor before each frame. Native
    /// backends can use the same signal when their page-flip scheduler lands.
    redraw_pending: bool,

    visual_profile: VisualProfile,
    top_bar_glass: GlassRecipe,
    connected_clients: u64,
    committed_surfaces: u64,

    decorations: Decorations,
    pressed_serials: PressedSerials,
    /// The window under an active compositor grab, if any.
    active_drag: Option<WindowId>,
    /// Serial that clients must echo for their move/resize to be honoured.
    last_grab_serial: Option<smithay::utils::Serial>,
    /// The last title-bar press, for double-click maximize detection.
    last_title_press: Option<(
        std::time::Instant,
        smithay::utils::Point<f64, smithay::utils::Logical>,
    )>,
    /// Live Liquid Material transitions, one per window at a time.
    animations: std::collections::HashMap<WindowId, (crate::anim::Transition, std::time::Instant)>,

    /// The dock, its render surface and per-item launch bounces.
    dock: crate::dock::DockModel,
    dock_renderer: DockRenderer,
    launch_bounces: std::collections::HashMap<String, std::time::Instant>,
    /// The compositor-owned terminal surface and its reusable texture.
    terminal_ui: terminal_ui::TerminalUi,
    terminal_renderer: terminal_render::TerminalRenderer,
    /// Real PTY sessions keyed by the stable id used by the visual tabs.
    terminal: crate::terminal::TerminalTabs,
    terminal_session_map: std::collections::HashMap<u64, crate::terminal::SessionId>,
    /// The welcome experience playing this session, if any.
    welcome: Option<crate::welcome::WelcomeStage>,
    welcome_started: std::time::Instant,
    welcome_renderer: WelcomeRenderer,
    /// Native Flatpak/Flathub gallery state and its host backend.
    app_store: crate::app_store::AppStore,
    app_store_backend: app_store_backend::FlatpakBackend,
    app_store_open: bool,
    app_store_renderer: app_store_render::AppStoreRenderer,
    app_store_last_refresh: Option<Instant>,
    /// Native notification queue and the Mac-like notification centre.
    notifications: crate::notifications::NotificationCenter,
    notifications_open: bool,
    notification_renderer: notification_render::NotificationRenderer,
    /// Resumable first-run environment setup shown after the welcome card.
    setup_state: crate::setup::SetupState,
    setup_open: bool,
    setup_renderer: setup_render::SetupRenderer,
    /// The desktop wallpaper, decoded once.
    wallpaper: Option<wallpaper::WallpaperRenderer>,
    /// app id per window, so the dock can group windows to launchers.
    window_apps: std::collections::HashMap<WindowId, String>,

    /// The shell: top bar, control centre and widget rail.
    shell_renderer: shell_render::ShellRenderer,
    widget_renderer: widget_render::WidgetRenderer,
    topbar_layout: crate::topbar::TopBarLayout,
    control_layout: crate::control::ControlLayout,
    control_open: bool,
    toggles: shell_render::ShellToggles,
    distribution: crate::topbar::Distribution,
    widget_board: crate::widgets::WidgetBoard,
    /// Cached hardware readings; CPU sampling sleeps, so refresh is timed.
    readings: backends::SystemReadings,
    readings_at: std::time::Instant,
    /// The widget press-and-hold in progress, if any.
    widget_hold: Option<(usize, std::time::Instant)>,
    /// The removal confirmation popup, while it is up.
    remove_popup: Option<crate::widgets::RemovePopup>,

    /// The application launcher sheet and its search text.
    launcher_open: bool,
    launcher_query: String,
    /// The parsed application database, scanned once at startup.
    applications: Vec<crate::desktop_entry::DesktopEntry>,
    /// When the entry database was last scanned, so the launcher can pick up a
    /// newly installed application without pacing a full rescan every frame.
    applications_scanned_at: Option<Instant>,

    /// The desktop icon area, mapped from the user's Desktop directory.
    desktop: crate::desktop_icons::Desktop,

    /// The Finder window, its state and items.
    finder_open: bool,
    finder: crate::finder::FinderState,
    finder_items: Vec<crate::finder::FinderItem>,
    finder_renderer: finder_render::FinderRenderer,

    /// The Settings window, its pane and the settings backend.
    settings_open: bool,
    settings_pane: crate::settings::Pane,
    settings_focus: crate::settings::SettingsFocus,
    settings_backend: settings_backend::SettingsBackend,
    settings_renderer: settings_render::SettingsRenderer,
    /// Pure detector/controller state; Linux collection is sampled on a
    /// paced timer and Settings receives only bounded explanations.
    game_mode_detector: crate::game_mode::GameDetector,
    game_mode_controller: crate::game_mode::GameModeController,
    game_mode_checked_at: Instant,
    game_mode_policy_active: bool,
    game_mode_manual_request: bool,

    /// The idle session state and the lock-screen password entry.
    session: crate::idle::Session,
    last_activity: std::time::Instant,
    password: crate::idle::PasswordEntry,

    /// The SF Pro font book, loaded once at startup.
    fonts: fonts::FontBook,

    /// Count of currently pressed pointer buttons, for rest-screen drags.
    pressed_buttons: usize,
}

#[derive(Clone)]
struct XdgWindow {
    id: WindowId,
    surface: ToplevelSurface,
    desktop: DesktopWindow,
    /// Whether the client's own size has been adopted once, after its first
    /// configured commit. The compositor's pre-map geometry is a placeholder.
    adopted_size: bool,
}

/// Visual preferences captured once around a Game Mode policy transition.
/// No process, input, audio, compositor, or security state is stored here.
impl Rouch {
    fn new(
        event_loop: &mut EventLoop<Self>,
        display: Display<Self>,
        output_size: Size<i32, Physical>,
        output_name: &str,
        output_transform: Transform,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let display_handle = display.handle();
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);
        let alpha_modifier_state =
            smithay::wayland::alpha_modifier::AlphaModifierState::new::<Self>(&display_handle);
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&display_handle);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&display_handle, "rouch");
        seat.add_keyboard(Default::default(), 200, 25)?;
        seat.add_pointer();

        let mode = Mode {
            size: output_size,
            refresh: 60_000,
        };
        let output = Output::new(
            output_name.to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Rouch".into(),
                model: "Rouch output".into(),
            },
        );
        let _output_global = output.create_global::<Self>(&display_handle);
        output.change_current_state(Some(mode), Some(output_transform), None, Some((0, 0).into()));
        output.set_preferred(mode);

        let mut space = Space::default();
        space.map_output(&output, (0, 0));
        let socket_name = Self::init_wayland_listener(display, event_loop)?;

        let setup_state = load_setup_state();

        Ok(Self {
            started_at: Instant::now(),
            socket_name,
            display_handle,
            compositor_state,
            xdg_shell_state,
            shm_state,
            _output_manager_state: output_manager_state,
            _alpha_modifier_state: alpha_modifier_state,
            data_device_state,
            primary_selection_state,
            _xdg_decoration_state: xdg_decoration_state,
            // The dmabuf global needs the renderer's format list, which only
            // exists once a backend has built one. `enable_dmabuf` publishes it.
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            seat_state,
            seat,
            windows: WindowManager::new(work_area_for(output_size)),
            xdg_windows: Vec::new(),
            popups: Vec::new(),
            output,
            space,
            loop_signal: event_loop.get_signal(),
            redraw_pending: true,
            visual_profile: VisualProfile::DEFAULT,
            top_bar_glass: glass_recipe(
                PerformanceProfile::default(),
                read_config_bool("reduce_transparency", false),
                GlassSurface::TopBar,
            ),
            connected_clients: 0,
            committed_surfaces: 0,
            decorations: Decorations::default(),
            pressed_serials: PressedSerials::default(),
            active_drag: None,
            last_grab_serial: None,
            last_title_press: None,
            animations: std::collections::HashMap::new(),
            dock: crate::dock::DockModel::new(),
            dock_renderer: DockRenderer::new(),
            launch_bounces: std::collections::HashMap::new(),
            terminal_ui: terminal_ui::TerminalUi::new(),
            terminal_renderer: terminal_render::TerminalRenderer::new(),
            terminal: crate::terminal::TerminalTabs::default(),
            terminal_session_map: std::collections::HashMap::new(),
            welcome: Self::detect_welcome_stage(),
            welcome_started: std::time::Instant::now(),
            welcome_renderer: WelcomeRenderer::new(),
            app_store: crate::app_store::AppStore::new(),
            app_store_backend: app_store_backend::FlatpakBackend::new(),
            app_store_open: false,
            app_store_renderer: app_store_render::AppStoreRenderer::new(),
            app_store_last_refresh: None,
            notifications: notification_center_from_config(),
            notifications_open: false,
            notification_renderer: notification_render::NotificationRenderer::new(),
            setup_open: !setup_state.is_complete(),
            setup_state,
            setup_renderer: setup_render::SetupRenderer::new(),
            wallpaper: wallpaper::find_wallpaper().and_then(|path| wallpaper::WallpaperRenderer::new(&path)),
            window_apps: std::collections::HashMap::new(),

            shell_renderer: shell_render::ShellRenderer::new(),
            widget_renderer: widget_render::WidgetRenderer::new(),
            topbar_layout: Self::initial_topbar_layout(work_area_for(output_size)),
            control_layout: crate::control::layout(work_area_for(output_size)),
            control_open: false,
            toggles: shell_render::ShellToggles {
                wifi: true,
                bluetooth: false,
                battery_saver: read_config_bool("battery_saver", false),
                focus: read_config_bool("focus", false),
                dark_mode: read_config_bool("dark_mode", false),
                brightness: 0.7,
                volume: 0.5,
            },
            distribution: {
                let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
                crate::topbar::Distribution::detect(&os_release)
            },
            widget_board: crate::widgets::WidgetBoard::default(),
            readings: backends::SystemReadings::read(),
            readings_at: std::time::Instant::now(),
            widget_hold: None,
            remove_popup: None,

            launcher_open: false,
            launcher_query: String::new(),
            applications: scan_applications(),
            applications_scanned_at: Some(Instant::now()),

            desktop: scan_desktop(),

            finder_open: false,
            finder: crate::finder::FinderState::new(&finder_backend::default_root()),
            finder_items: Vec::new(),
            finder_renderer: finder_render::FinderRenderer::new(),

            settings_open: false,
            settings_pane: crate::settings::Pane::System,
            settings_focus: crate::settings::first_focus(),
            settings_backend: settings_backend::SettingsBackend::new(),
            settings_renderer: settings_render::SettingsRenderer::new(),
            game_mode_detector: crate::game_mode::GameDetector::default(),
            game_mode_controller: crate::game_mode::GameModeController::default(),
            game_mode_checked_at: Instant::now()
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or_else(Instant::now),
            game_mode_policy_active: false,
            game_mode_manual_request: false,

            session: crate::idle::Session::Active,
            last_activity: std::time::Instant::now(),
            password: crate::idle::PasswordEntry::new(),

            fonts: fonts::FontBook::load(),
            pressed_buttons: 0,
        })
    }

    fn initial_topbar_layout(work_area: Rect) -> crate::topbar::TopBarLayout {
        let clock = crate::topbar::clock_text(0);
        crate::topbar::layout(work_area, 70, pixel::text_width(&clock))
    }

    /// Refresh the cached readings at a paced interval. The CPU sampler
    /// sleeps, so it must not run per frame.
    pub fn refresh_readings(&mut self) {
        let _ = self.notifications.expire(self.notification_now_ms());
        let reading_interval = if self.game_mode_policy_active {
            std::time::Duration::from_secs(2)
        } else {
            std::time::Duration::from_millis(750)
        };
        if self.readings_at.elapsed() >= reading_interval {
            self.readings = backends::SystemReadings::read();
            self.readings_at = std::time::Instant::now();
        }

        // Game detection is intentionally much slower than pointer/render
        // work. One bounded `/proc` sample every two seconds is enough to
        // notice a game entering or leaving the foreground without turning
        // process inspection into a frame-loop cost.
        let _ = self.refresh_game_mode_if_due();

        // An in-progress widget hold is evaluated per frame so edit mode
        // engages exactly at the threshold, mid-press.
        if let Some((index, started)) = self.widget_hold {
            if started.elapsed() >= crate::widgets::HOLD_THRESHOLD && !self.widget_board.editing {
                self.widget_board.set_editing(true);
            }
            let _ = index;
        }
    }

    /// Run the bounded Game Mode sample only after its two-second cadence.
    /// The nested timer calls this even when the desktop has no redraw work,
    /// so leaving a game can restore the shell without pointer activity.
    pub fn refresh_game_mode_if_due(&mut self) -> bool {
        if self.game_mode_checked_at.elapsed() < std::time::Duration::from_secs(2) {
            return false;
        }
        self.refresh_game_mode()
    }

    /// Seconds since the Unix epoch, for clocks and the widget rail.
    pub fn now_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Monotonic session time used by notification expiration and rendering.
    pub fn notification_now_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    /// Queue one native notification from a compositor integration or a shell
    /// operation. The centre itself decides whether it is delivered, deferred
    /// by DND, or suppressed by the user's setting.
    pub fn notify(
        &mut self,
        notification: crate::notifications::Notification,
    ) -> crate::notifications::EnqueueResult {
        self.notifications.enqueue(notification)
    }

    /// Toggle the Mac-like notification centre from the menu-bar clock.
    pub fn toggle_notifications(&mut self) {
        self.notifications_open = !self.notifications_open;
        if self.notifications_open {
            self.control_open = false;
            self.app_store_open = false;
            self.launcher_open = false;
            self.finder_open = false;
            self.settings_open = false;
            let _ = self.notifications.expire(self.notification_now_ms());
        }
    }

    /// Close the notification centre without changing queued alerts.
    pub fn close_notifications(&mut self) {
        self.notifications_open = false;
    }

    /// Open the native app gallery and refresh it once on entry.
    pub fn toggle_app_store(&mut self) {
        self.app_store_open = !self.app_store_open;
        if self.app_store_open {
            self.launcher_open = false;
            self.finder_open = false;
            self.settings_open = false;
            self.notifications_open = false;
            self.control_open = false;
            let should_refresh = self
                .app_store_last_refresh
                .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(30));
            if should_refresh {
                self.refresh_app_store();
            }
        }
    }

    /// Query Flathub through the backend, retaining a cache/local fallback on
    /// hosts without Flatpak or network access.
    pub fn refresh_app_store(&mut self) {
        // Desktop entries are an honest last-resort catalogue. They make the
        // gallery useful on a fresh host without Flatpak while keeping the
        // action as Open instead of pretending a local package is installable.
        let local_fallback = self
            .applications
            .iter()
            .map(|entry| {
                let mut app = crate::app_store::FlatpakApp::new(
                    entry.id.clone(),
                    entry.name.clone(),
                    "Installed desktop application",
                );
                app.installed = true;
                app.metadata.categories = entry.categories.clone();
                app.metadata.origin = Some("local-desktop-entry".to_owned());
                app
            })
            .collect();
        self.app_store_backend.set_local_fallback(local_fallback);
        self.app_store_last_refresh = Some(Instant::now());
        self.app_store.set_loading();
        let snapshot = self.app_store_backend.refresh();
        let state = snapshot.state;
        let message = snapshot.message;
        match snapshot.source {
            crate::app_store::CatalogSource::Live => self.app_store.set_apps(snapshot.apps),
            crate::app_store::CatalogSource::Cache | crate::app_store::CatalogSource::Local => {
                self.app_store.set_cached_apps(snapshot.apps)
            }
        }
        match state {
            crate::app_store::CatalogState::Error(error) => {
                self.app_store.set_error(message.unwrap_or(error));
            }
            crate::app_store::CatalogState::Offline => self.app_store.set_offline(),
            crate::app_store::CatalogState::Ready
            | crate::app_store::CatalogState::Empty
            | crate::app_store::CatalogState::Loading => {}
        }
    }

    /// Type into the gallery search field.
    pub fn app_store_type(&mut self, text: &str) {
        self.app_store.query.push_str(text);
        self.app_store.query.truncate(64);
        self.app_store.clear_selection();
    }

    /// Delete one character from the gallery search field.
    pub fn app_store_backspace(&mut self) {
        self.app_store.query.pop();
        self.app_store.clear_selection();
    }

    /// Activate a gallery card. The first click selects; a second click runs
    /// the explicit install/update/open action shown on the card.
    pub fn app_store_activate(&mut self, index: usize) -> bool {
        let filtered = self.app_store.filtered();
        let Some(app) = filtered.get(index) else {
            return false;
        };
        let app_id = app.app_id().to_owned();
        let action = app.primary_action();
        if self.app_store.selected != Some(index) {
            return self.app_store.select(index);
        }

        match action {
            crate::app_store::AppAction::Install => {
                let report = self.app_store_backend.install(&app_id);
                self.notify_operation_report("Install", &app_id, &report);
            }
            crate::app_store::AppAction::Update => {
                let report = self.app_store_backend.update(&app_id);
                self.notify_operation_report("Update", &app_id, &report);
            }
            crate::app_store::AppAction::Open => {
                if self.applications.iter().any(|entry| entry.id == app_id) {
                    self.launch(&app_id);
                } else {
                    self.launch_flatpak(&app_id);
                }
            }
        }
        self.refresh_app_store();
        true
    }

    fn notify_operation_report(
        &mut self,
        operation: &str,
        app_id: &str,
        report: &app_store_backend::OperationReport,
    ) {
        let level = if report.is_success() {
            crate::notifications::NotificationLevel::Success
        } else if report.is_unavailable() {
            crate::notifications::NotificationLevel::Warning
        } else {
            crate::notifications::NotificationLevel::Error
        };
        let title = if report.is_success() {
            format!("{operation} complete")
        } else {
            format!("{operation} unavailable")
        };
        let body = if report.is_success() {
            app_id.to_owned()
        } else {
            report.message.clone()
        };
        let _ = self.notify(crate::notifications::Notification::new(
            "rouch.app-gallery",
            title,
            body,
            level,
            self.notification_now_ms(),
        ));
    }

    fn launch_flatpak(&mut self, app_id: &str) {
        if app_store_backend::validate_app_id(app_id).is_err() {
            return;
        }
        let mut command = std::process::Command::new("flatpak");
        command.args(["run", "--user", app_id]);
        self.prepare_client_command(&mut command);
        let result = command.spawn();
        if let Err(error) = result {
            warn!(app_id, ?error, "Could not open Flatpak application");
            let _ = self.notify(crate::notifications::Notification::new(
                "rouch.app-gallery",
                "Could not open application",
                app_id,
                crate::notifications::NotificationLevel::Error,
                self.notification_now_ms(),
            ));
        }
    }

    /// The first-run setup card's primary action.
    pub fn setup_continue(&mut self) -> bool {
        let action = match self.setup_state.status {
            crate::setup::SetupStatus::NotStarted => crate::setup::SetupAction::Start,
            crate::setup::SetupStatus::InProgress => crate::setup::SetupAction::Complete,
            crate::setup::SetupStatus::Failed => crate::setup::SetupAction::Recover,
            crate::setup::SetupStatus::Complete => {
                self.setup_open = false;
                return true;
            }
        };
        match self.setup_state.apply(action) {
            Ok(_) => {
                self.setup_open = !self.setup_state.is_complete();
                self.persist_setup_state();
            }
            Err(error) => {
                warn!(?error, "Rouch setup action could not be applied");
                let _ = self.setup_state.fail(error.to_string());
                self.persist_setup_state();
            }
        }
        true
    }

    /// Skip only optional setup steps; required steps remain modal.
    pub fn setup_skip(&mut self) -> bool {
        match self.setup_state.apply(crate::setup::SetupAction::Skip) {
            Ok(_) => {
                self.setup_open = !self.setup_state.is_complete();
                self.persist_setup_state();
            }
            Err(error) => {
                let _ = self.notify(crate::notifications::Notification::new(
                    "rouch.setup",
                    "This setup step is required",
                    error.to_string(),
                    crate::notifications::NotificationLevel::Info,
                    self.notification_now_ms(),
                ));
            }
        }
        true
    }

    fn persist_setup_state(&self) {
        let path = setup_state_path();
        let Ok(plan) = self.setup_state.persistence_plan(path.clone()) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(mut file) = std::fs::File::create(&plan.temporary_path) else {
            return;
        };
        use std::io::Write;
        if file.write_all(plan.contents.as_bytes()).is_ok() && file.sync_all().is_ok() {
            let _ = std::fs::rename(&plan.temporary_path, &plan.path);
        }
    }

    /// Update every layout that depends on the work area or live texts.
    pub fn relayout_shell(&mut self) {
        let work_area = self.windows.work_area();
        let clock = crate::topbar::clock_text(self.now_secs());
        let app_name = self.active_app_name();
        self.topbar_layout =
            crate::topbar::layout(work_area, pixel::text_width(&app_name), pixel::text_width(&clock));
        self.control_layout = crate::control::layout(work_area);
    }

    /// The active application's name for the bar.
    fn active_app_name(&self) -> String {
        if self.terminal_ui.open && self.terminal_ui.focused {
            return "Terminal".into();
        }
        let focused = self
            .windows
            .visible_windows()
            .into_iter()
            .rev()
            .find(|window| window.active() && !window.minimized());
        match focused {
            Some(window) => self
                .window_apps
                .get(&window.id())
                .cloned()
                .map(|app| app.split('.').next_back().unwrap_or(&app).to_owned())
                .unwrap_or_else(|| "Rouch".into()),
            None => "Rouch".into(),
        }
    }

    /// A press landed on the top bar: open a shell surface or the centre.
    pub fn press_topbar(&mut self, item: crate::topbar::TopBarItem) -> bool {
        match item {
            crate::topbar::TopBarItem::ControlCentre => {
                self.control_open = !self.control_open;
                if self.control_open {
                    self.app_store_open = false;
                    self.notifications_open = false;
                    self.launcher_open = false;
                    self.finder_open = false;
                    self.settings_open = false;
                }
                true
            }
            // The distribution mark opens the application launcher, the
            // Tahoe menu that replaced Launchpad.
            crate::topbar::TopBarItem::DistributionMark => {
                self.toggle_launcher();
                true
            }
            // The clock flips the widget rail visibility, like the Tahoe
            // menu-bar clock toggles widgets.
            crate::topbar::TopBarItem::Clock => {
                self.widget_board.set_editing(false);
                self.toggle_notifications();
                true
            }
            _ => false,
        }
    }

    /// Toggle a control from the open panel; sliders set values.
    pub fn apply_control(&mut self, control: crate::control::Control, value: Option<f32>) {
        let toggles = &mut self.toggles;
        match control {
            crate::control::Control::Wifi => toggles.wifi = !toggles.wifi,
            crate::control::Control::Bluetooth => toggles.bluetooth = !toggles.bluetooth,
            crate::control::Control::BatterySaver => {
                toggles.battery_saver = !toggles.battery_saver;
                // Battery Saver also pins the glass policy, like the design rules.
                self.top_bar_glass = crate::design::glass_recipe(
                    if toggles.battery_saver {
                        crate::design::PerformanceProfile::BatterySaver
                    } else {
                        crate::design::PerformanceProfile::Balanced
                    },
                    false,
                    crate::design::GlassSurface::TopBar,
                );
                self.settings_renderer.set_opaque_fallback(
                    self.game_mode_policy_active
                        || toggles.battery_saver
                        || read_config_bool("reduce_transparency", false),
                );
            }
            crate::control::Control::Focus => toggles.focus = !toggles.focus,
            crate::control::Control::DarkMode => toggles.dark_mode = !toggles.dark_mode,
            crate::control::Control::Brightness => {
                if let Some(value) = value {
                    toggles.brightness = value;
                }
            }
            crate::control::Control::Volume => {
                if let Some(value) = value {
                    toggles.volume = value;
                }
            }
        }
    }

    /// Begin (or continue) a widget press-and-hold; returns true when the
    /// hold crossed the threshold and edit mode engaged.
    pub fn widget_hold_tick(&mut self, index: usize, now: std::time::Instant) -> bool {
        match self.widget_hold {
            Some((held_index, _)) if held_index == index => {
                let held = now.duration_since(self.widget_hold.unwrap().1) >= crate::widgets::HOLD_THRESHOLD;
                if held && !self.widget_board.editing {
                    self.widget_board.set_editing(true);
                }
                held
            }
            _ => {
                self.widget_hold = Some((index, now));
                false
            }
        }
    }

    /// End a widget hold; true when it was a tap, not a hold.
    pub fn widget_release(&mut self, index: usize) -> bool {
        let held = self.widget_hold.take();
        matches!(held, Some((held_index, _)) if held_index == index && !self.widget_board.editing)
    }

    /// The remove-confirmation popup opened for a widget.
    pub fn open_remove_popup(&mut self, index: usize) {
        let Some(slot) = self.widget_board.slots.get(index) else {
            return;
        };
        self.remove_popup = Some(crate::widgets::RemovePopup {
            kind: slot.kind,
            index,
        });
    }

    /// Resolve a press inside the removal popup.
    pub fn popup_press(&mut self, point: RouchPoint) -> bool {
        let Some(popup) = self.remove_popup.clone() else {
            return false;
        };
        let board_rect = crate::widgets::WidgetBoard::board_rect(self.windows.work_area());
        let anchor = self
            .widget_board
            .slots
            .get(popup.index)
            .map(|slot| slot.rect(board_rect))
            .unwrap_or(board_rect);
        let rect = crate::widgets::popup_rect(anchor, self.windows.work_area());

        match crate::widgets::popup_hit(rect, point) {
            crate::widgets::PopupHit::Remove => {
                let removed = self.widget_board.remove(popup.index);
                self.remove_popup = None;
                if let Some(kind) = removed {
                    info!(widget = kind.name(), "Widget removed from the board");
                }
                if self.widget_board.slots.is_empty() {
                    self.widget_board.set_editing(false);
                }
                true
            }
            crate::widgets::PopupHit::Cancel => {
                self.remove_popup = None;
                true
            }
            // Outside presses dismiss the popup and stay in edit mode, like
            // the iPhone home-screen sheet.
            crate::widgets::PopupHit::Outside => {
                self.remove_popup = None;
                true
            }
        }
    }

    // ----- Launcher ------------------------------------------------------

    /// Toggle the application launcher sheet.
    pub fn toggle_launcher(&mut self) {
        self.launcher_open = !self.launcher_open;
        if self.launcher_open {
            // An application installed during this session must show up here
            // without a relogin.
            self.refresh_applications();
            self.launcher_query.clear();
            self.app_store_open = false;
            self.notifications_open = false;
            self.finder_open = false;
            self.settings_open = false;
            self.control_open = false;
        }
    }

    /// Type into the launcher search field.
    pub fn launcher_type(&mut self, text: &str) {
        self.launcher_query.push_str(text);
        self.launcher_query.truncate(40);
    }

    /// Backspace the launcher query.
    pub fn launcher_backspace(&mut self) {
        self.launcher_query.pop();
    }

    /// Launch the application at one filtered launcher index.
    pub fn launcher_activate(&mut self, index: usize) -> bool {
        let app_id = crate::launcher::filter_apps(&self.applications, &self.launcher_query)
            .get(index)
            .map(|app| app.id.clone());
        let Some(app_id) = app_id else {
            return false;
        };
        self.launcher_open = false;
        self.launch(&app_id);
        true
    }

    /// Launch an application by desktop id, bouncing it in the dock.
    pub fn launch(&mut self, app_id: &str) {
        self.launch_bounces.insert(app_id.to_owned(), Instant::now());
        let Some(entry) = self.applications.iter().find(|app| app.id == app_id).cloned() else {
            warn!(app_id, "Launch requested for an unknown application");
            return;
        };
        let (program, args) = entry.launch_parts();
        if program.is_empty() {
            return;
        }
        info!(app_id, program, "Launching application");
        let mut command = std::process::Command::new(&program);
        command.args(&args);
        self.prepare_client_command(&mut command);
        if let Err(error) = command.spawn() {
            warn!(program, ?error, "Could not launch application");
        }
    }

    /// Configure one child process to run as a native Wayland client.
    ///
    /// A toolkit only selects its Wayland backend when it is told to. Qt in
    /// particular defaults to X11 and simply exits when no X server answers,
    /// so a desktop that sets `WAYLAND_DISPLAY` alone cannot start half the
    /// applications a user has installed.
    fn prepare_client_command(&self, command: &mut std::process::Command) {
        command
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env("XDG_SESSION_TYPE", "wayland")
            .env("XDG_CURRENT_DESKTOP", CURRENT_DESKTOP)
            .env("XDG_SESSION_DESKTOP", CURRENT_DESKTOP)
            // `wayland;xcb` keeps the X11 path available for the day XWayland
            // is bridged, without making it the first choice today.
            .env("QT_QPA_PLATFORM", "wayland;xcb")
            .env("GDK_BACKEND", "wayland,x11")
            .env("SDL_VIDEODRIVER", "wayland")
            .env("CLUTTER_BACKEND", "wayland")
            .env("MOZ_ENABLE_WAYLAND", "1")
            .env("ELECTRON_OZONE_PLATFORM_HINT", "auto")
            // Rouch draws every title bar itself; a Qt client that also draws
            // one would stack two.
            .env("QT_WAYLAND_DISABLE_WINDOWDECORATION", "1")
            // Reparenting window managers confuse AWT into a blank frame.
            .env("_JAVA_AWT_WM_NONREPARENTING", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // A `DISPLAY` inherited from the display manager points at an X server
        // that is not ours. Until XWayland is bridged, leaving it set makes a
        // toolkit prefer a server it cannot reach.
        command.env_remove("DISPLAY");
    }

    // ----- Finder --------------------------------------------------------

    /// Toggle the Finder window, refreshing its listing on open.
    pub fn toggle_finder(&mut self) {
        self.finder_open = !self.finder_open;
        if self.finder_open {
            self.app_store_open = false;
            self.notifications_open = false;
            self.launcher_open = false;
            self.settings_open = false;
            self.control_open = false;
            self.refresh_finder();
        }
    }

    /// Reload the current Finder directory into the item list.
    pub fn refresh_finder(&mut self) {
        self.finder_items = finder_backend::list_dir(self.finder.current(), false);
        finder_backend::sort_items(&mut self.finder_items, self.finder.sort);
    }

    /// Navigate the Finder, refreshing the listing.
    pub fn finder_navigate(&mut self, path: &str) {
        self.finder.navigate(path);
        self.refresh_finder();
    }

    /// The Finder's back/forward.
    pub fn finder_back(&mut self) {
        if self.finder.back() {
            self.refresh_finder();
        }
    }

    pub fn finder_forward(&mut self) {
        if self.finder.forward_() {
            self.refresh_finder();
        }
    }

    /// Type into the Finder search field.
    pub fn finder_type(&mut self, text: &str) {
        self.finder.query.push_str(text);
        self.finder.query.truncate(40);
    }

    pub fn finder_backspace(&mut self) {
        self.finder.query.pop();
    }

    /// Activate a Finder item: folders navigate, files open externally.
    pub fn finder_activate(&mut self, index: usize) -> bool {
        let visible = self.finder.filtered(&self.finder_items);
        let Some(item) = visible.get(index).cloned() else {
            return false;
        };
        let current = self.finder.current().to_owned();
        let path = std::path::Path::new(&current).join(&item.name);
        if let Some(dir) = finder_backend::open_path(&path) {
            self.finder_navigate(&dir);
            return true;
        }
        true
    }

    // ----- Settings ------------------------------------------------------

    /// Toggle the Settings window.
    pub fn toggle_settings(&mut self) {
        self.settings_open = !self.settings_open;
        if self.settings_open {
            self.app_store_open = false;
            self.notifications_open = false;
            self.launcher_open = false;
            self.finder_open = false;
            self.control_open = false;
            self.settings_focus = crate::settings::first_focus();
            self.settings_renderer.set_focus(Some(self.settings_focus));
            self.settings_renderer.set_opaque_fallback(
                self.game_mode_policy_active || read_config_bool("reduce_transparency", false),
            );
        } else {
            self.settings_renderer.set_focus(None);
        }
        self.request_redraw();
    }

    /// Switch the settings pane.
    pub fn settings_pane(&mut self, pane: crate::settings::Pane) {
        self.settings_pane = pane;
        let index = crate::settings::Pane::ALL
            .iter()
            .position(|candidate| *candidate == pane)
            .unwrap_or(0);
        self.settings_focus = crate::settings::SettingsFocus::Sidebar(index);
        self.settings_renderer.set_focus(Some(self.settings_focus));
        self.request_redraw();
    }

    /// Snapshot the active Settings pane for keyboard routing and activation.
    fn settings_rows(&self) -> Vec<crate::settings::Setting> {
        let profile = crate::user::UserProfile {
            name: self.profile_name(),
            avatar: crate::user::AvatarSource::default(),
        };
        self.settings_backend
            .pane_rows(self.settings_pane, &profile, &self.toggles)
    }

    /// Move the Settings focus ring without allocating a focus list.
    pub fn settings_focus_next(&mut self, reverse: bool) -> crate::settings::SettingsFocus {
        let rows = self.settings_rows();
        self.settings_focus = crate::settings::cycle_focus(self.settings_focus, &rows, reverse);
        self.settings_renderer.set_focus(Some(self.settings_focus));
        self.request_redraw();
        self.settings_focus
    }

    pub const fn settings_focus(&self) -> crate::settings::SettingsFocus {
        self.settings_focus
    }

    /// Activate the current keyboard target. A manual Game Mode preview is
    /// therefore always an explicit user action, never a detector side effect.
    pub fn settings_activate_focused(&mut self) -> bool {
        match self.settings_focus {
            crate::settings::SettingsFocus::Sidebar(index) => {
                let Some(pane) = crate::settings::Pane::ALL.get(index).copied() else {
                    return false;
                };
                self.settings_pane(pane);
                true
            }
            crate::settings::SettingsFocus::Row(index) => {
                let rows = self.settings_rows();
                let Some(setting) = rows.get(index) else {
                    return false;
                };
                match setting {
                    crate::settings::Setting::Toggle { key, value, .. } => {
                        self.settings_apply(key, crate::settings::SettingValue::Bool(!value));
                        true
                    }
                    crate::settings::Setting::Select {
                        key,
                        options,
                        selected,
                        ..
                    } => {
                        let next = (*selected + 1) % options.len().max(1);
                        self.settings_apply(key, crate::settings::SettingValue::Choice(next));
                        true
                    }
                    crate::settings::Setting::Action { key, .. } => {
                        self.settings_apply(key, crate::settings::SettingValue::Bool(true));
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// Adjust a focused select or slider with an arrow-key-sized delta.
    pub fn settings_adjust_focused(&mut self, delta: i8) -> bool {
        let crate::settings::SettingsFocus::Row(index) = self.settings_focus else {
            return false;
        };
        let rows = self.settings_rows();
        let Some(setting) = rows.get(index) else {
            return false;
        };
        match setting {
            crate::settings::Setting::Select {
                key,
                options,
                selected,
                ..
            } if !options.is_empty() => {
                let len = options.len();
                let next = if delta.is_negative() {
                    selected
                        .checked_sub(delta.unsigned_abs() as usize)
                        .unwrap_or(len - 1)
                } else {
                    selected.saturating_add(delta as usize) % len
                };
                self.settings_apply(key, crate::settings::SettingValue::Choice(next));
                true
            }
            crate::settings::Setting::Slider {
                key, value, min, max, ..
            } => {
                let step = ((*max - *min).abs() / 20.0).max(0.01);
                let next = (*value + step * delta as f32).clamp(*min, *max);
                self.settings_apply(key, crate::settings::SettingValue::Float(next));
                true
            }
            _ => false,
        }
    }

    /// Sample Linux evidence, score the foreground workload and reconcile the
    /// reversible shell policy. Process inspection is paced by
    /// `refresh_readings`; it never runs in the pointer or render hot path.
    pub fn refresh_game_mode(&mut self) -> bool {
        let previous = self.settings_backend.game_mode_snapshot().clone();
        let (windows, foreground_pid) = self.game_mode_window_snapshots();
        let desktop_entries = self
            .applications
            .iter()
            .take(crate::game_mode::MAX_DESKTOP_ENTRIES)
            .map(|entry| crate::game_mode::DesktopEntrySnapshot {
                desktop_id: entry.id.clone(),
                categories: entry.categories.clone(),
                app_id: Some(entry.id.clone()),
                wm_class: None,
                pid: None,
            })
            .collect();
        let snapshot = game_mode_backend::collect_snapshot(windows, desktop_entries, foreground_pid);
        let detection = self.game_mode_detector.detect(&snapshot);
        let choice = previous.choice;
        self.game_mode_controller.set_preference(choice.core_preference());
        self.game_mode_controller
            .set_tier(game_mode_backend::hardware_tier());

        let transition = self.game_mode_controller.reconcile(
            &detection,
            self.current_game_mode_shell_state(),
            self.game_mode_manual_request,
        );
        let mut transition_error = None;
        match transition {
            Ok(crate::game_mode::GameModeTransition::Entered { state, .. }) => {
                self.apply_game_mode_shell_state(state, true);
            }
            Ok(crate::game_mode::GameModeTransition::Exited { restored }) => {
                self.apply_game_mode_shell_state(restored, false);
                self.game_mode_manual_request = false;
            }
            Ok(crate::game_mode::GameModeTransition::NoChange { active }) => {
                self.game_mode_policy_active = active;
            }
            Err(error) => {
                warn!(?error, "Game Mode policy was not applied");
                transition_error = Some(error);
            }
        }

        let policy = crate::game_mode::GameModePolicy::for_tier(self.game_mode_controller.tier());
        let mut presented = crate::settings::GameModeSnapshot::from_detection(
            &detection,
            choice,
            policy,
            self.game_mode_policy_active,
            self.game_mode_controller.cache_full(),
        );
        if let Some(error) = transition_error {
            presented.apply_error = Some(format!("{error:?}"));
        }
        let changed = previous != presented;
        self.settings_backend.set_game_mode_snapshot(presented);
        self.game_mode_checked_at = Instant::now();
        if changed {
            self.request_redraw();
        }
        changed
    }

    /// Build the window/process association used by the detector. Wayland
    /// client credentials provide the foreground PID when the protocol socket
    /// exposes it; if they are unavailable, runtime evidence is ignored rather
    /// than attributed to every process on the machine.
    fn game_mode_window_snapshots(&self) -> (Vec<crate::game_mode::WindowSnapshot>, Option<u32>) {
        let foreground_id = self.windows.active_window();
        let mut foreground_pid = None;
        let windows = self
            .xdg_windows
            .iter()
            .filter_map(|binding| {
                let model = self.windows.window(binding.id)?;
                let pid = surface_client_pid(&self.display_handle, &binding.surface);
                if Some(binding.id) == foreground_id {
                    foreground_pid = pid;
                }
                let app_id = self
                    .window_apps
                    .get(&binding.id)
                    .cloned()
                    .or_else(|| surface_app_id(&binding.surface));
                Some(crate::game_mode::WindowSnapshot {
                    window_id: binding.id.raw(),
                    pid,
                    app_id,
                    wm_class: None,
                    fullscreen: model.fullscreen(),
                    focused: model.active(),
                })
            })
            .take(crate::game_mode::MAX_WINDOW_SNAPSHOTS)
            .collect();
        (windows, foreground_pid)
    }

    fn current_game_mode_shell_state(&self) -> crate::game_mode::ShellState {
        crate::game_mode::ShellState {
            battery_saver: self.toggles.battery_saver,
            blur_enabled: self.top_bar_glass.blur_radius_px > 0,
            transparency_enabled: !self.top_bar_glass.opaque_content,
            // These values describe optional shell work. They are restored
            // from the typed controller snapshot and are not process policy.
            background_reads_enabled: !self.game_mode_policy_active,
            widget_refresh_enabled: !self.game_mode_policy_active,
            gallery_refresh_enabled: !self.game_mode_policy_active,
            redraw_fps: 0,
            animations_enabled: !self.game_mode_policy_active,
            noncritical_notifications_enabled: !self.game_mode_policy_active,
        }
    }

    fn apply_game_mode_shell_state(&mut self, state: crate::game_mode::ShellState, active: bool) {
        self.game_mode_policy_active = active;
        self.toggles.battery_saver = state.battery_saver;
        self.top_bar_glass = crate::design::glass_recipe(
            if state.battery_saver {
                crate::design::PerformanceProfile::BatterySaver
            } else {
                crate::design::PerformanceProfile::Balanced
            },
            !state.transparency_enabled,
            crate::design::GlassSurface::TopBar,
        );
        self.settings_renderer
            .set_opaque_fallback(active || !state.transparency_enabled);
    }

    /// Accept a presentation snapshot from another Linux integration. The
    /// normal nested path calls `refresh_game_mode`; this method remains a
    /// narrow boundary for native adapters and tests.
    pub fn update_game_mode_snapshot(&mut self, snapshot: crate::settings::GameModeSnapshot) {
        self.game_mode_controller
            .set_preference(snapshot.choice.core_preference());
        self.settings_backend.set_game_mode_snapshot(snapshot);
        self.request_redraw();
    }

    /// Preview the already-reported evidence through the real controller.
    /// Preview never claims to control processes or kernel scheduling.
    pub fn preview_game_mode(&mut self) -> bool {
        let snapshot = self.settings_backend.game_mode_snapshot().clone();
        if !crate::settings::game_mode_preview_allowed(&snapshot) {
            let _ = self.notify(crate::notifications::Notification::new(
                "rouch.game-mode",
                "Game Mode preview unavailable",
                "No trusted foreground-game evidence is available; nothing was changed.",
                crate::notifications::NotificationLevel::Warning,
                self.notification_now_ms(),
            ));
            return false;
        }
        if snapshot.choice == crate::settings::GameModeChoice::On {
            self.game_mode_manual_request = true;
        }
        self.game_mode_checked_at = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3))
            .unwrap_or_else(Instant::now);
        self.refresh_game_mode()
    }

    /// Restore the typed pre-game shell state exactly once. In Auto mode a
    /// later sample may intentionally enter again if the same game remains in
    /// the foreground.
    pub fn restore_game_mode(&mut self) -> bool {
        let was_active = self.game_mode_controller.is_active() || self.game_mode_policy_active;
        self.game_mode_manual_request = false;
        if let Ok(crate::game_mode::GameModeTransition::Exited { restored }) =
            self.game_mode_controller.exit()
        {
            self.apply_game_mode_shell_state(restored, false);
        } else {
            self.game_mode_policy_active = false;
            self.settings_renderer
                .set_opaque_fallback(read_config_bool("reduce_transparency", false));
        }
        let mut snapshot = self.settings_backend.game_mode_snapshot().clone();
        snapshot.policy_active = false;
        snapshot.cache_occupied = self.game_mode_controller.cache_full();
        snapshot.apply_error = None;
        self.settings_backend.set_game_mode_snapshot(snapshot);
        self.request_redraw();
        was_active
    }

    pub const fn game_mode_policy_active(&self) -> bool {
        self.game_mode_policy_active
    }

    /// Apply a setting change to the real system.
    pub fn settings_apply(&mut self, key: &str, value: crate::settings::SettingValue) {
        let pane = self.settings_pane;
        if let Some(applied) = self.settings_backend.apply(pane, key, value.clone()) {
            self.reflect_setting(key, &applied);
            debug!(pane = ?pane, key, ?applied, "Setting applied");
        }
    }

    /// Mirror a settings change into the shell's live state.
    fn reflect_setting(&mut self, key: &str, value: &crate::settings::SettingValue) {
        match key {
            "dark_mode" => {
                if let crate::settings::SettingValue::Bool(on) = value {
                    self.toggles.dark_mode = *on;
                }
            }
            "focus" => {
                if let crate::settings::SettingValue::Bool(on) = value {
                    self.toggles.focus = *on;
                }
            }
            "battery_saver" => {
                if let crate::settings::SettingValue::Bool(on) = value {
                    self.toggles.battery_saver = *on;
                    if !self.game_mode_policy_active {
                        self.top_bar_glass = crate::design::glass_recipe(
                            if *on {
                                crate::design::PerformanceProfile::BatterySaver
                            } else {
                                crate::design::PerformanceProfile::Balanced
                            },
                            read_config_bool("reduce_transparency", false),
                            crate::design::GlassSurface::TopBar,
                        );
                    }
                }
            }
            "notifications.enabled" => {
                if let crate::settings::SettingValue::Bool(on) = value {
                    self.notifications.preferences_mut().set_enabled(*on);
                    if !*on {
                        self.notifications_open = false;
                    }
                }
            }
            "notifications.dnd" => {
                if let crate::settings::SettingValue::Bool(on) = value {
                    self.notifications.preferences_mut().set_do_not_disturb(*on);
                }
            }
            "notifications.clear" => {
                self.notifications.clear();
                self.notifications_open = false;
            }
            "reduce_transparency" => {
                if let crate::settings::SettingValue::Bool(on) = value {
                    if !self.game_mode_policy_active {
                        self.top_bar_glass = crate::design::glass_recipe(
                            if self.toggles.battery_saver {
                                crate::design::PerformanceProfile::BatterySaver
                            } else {
                                crate::design::PerformanceProfile::Balanced
                            },
                            *on,
                            crate::design::GlassSurface::TopBar,
                        );
                    }
                    self.settings_renderer
                        .set_opaque_fallback(self.game_mode_policy_active || *on);
                }
            }
            "swap.recommend" => {
                let body = self.settings_backend.swap_recommendation();
                let _ = self.notify(crate::notifications::Notification::new(
                    "rouch.performance",
                    "Swapfile recommendation",
                    body,
                    crate::notifications::NotificationLevel::Info,
                    self.notification_now_ms(),
                ));
            }
            "brightness" => {
                if let crate::settings::SettingValue::Float(level) = value {
                    self.toggles.brightness = *level;
                }
            }
            "volume" => {
                if let crate::settings::SettingValue::Float(level) = value {
                    self.toggles.volume = *level;
                }
            }
            "reset_board" => {
                self.widget_board = crate::widgets::WidgetBoard::default();
            }
            "game_mode.mode" => {
                if let crate::settings::SettingValue::Choice(index) = value {
                    let choice = crate::settings::GameModeChoice::from_index(*index);
                    self.game_mode_manual_request = false;
                    self.game_mode_controller.set_preference(choice.core_preference());
                    if choice == crate::settings::GameModeChoice::Off {
                        self.restore_game_mode();
                    } else {
                        let _ = self.refresh_game_mode();
                    }
                }
            }
            "game_mode.preview" => {
                let _ = self.preview_game_mode();
            }
            "game_mode.restore" => {
                let _ = self.restore_game_mode();
            }
            _ => {}
        }
    }

    // ----- Session (idle, rest, lock) ------------------------------------

    /// Register user activity; a blank session wakes.
    pub fn note_activity(&mut self) {
        self.last_activity = std::time::Instant::now();
    }

    /// Evaluate the idle chain. Returns true when the session blanked.
    pub fn tick_session(&mut self) -> bool {
        match self.session {
            crate::idle::Session::Active if self.last_activity.elapsed() >= crate::idle::IDLE_TIMEOUT => {
                self.session = crate::idle::Session::Blank;
                true
            }
            _ => false,
        }
    }

    /// A press: wake a blank session into the rest screen.
    pub fn session_press(&mut self) -> bool {
        self.note_activity();
        let before = self.session;
        self.session = self.session.on_press();
        before != self.session
    }

    /// A key: advance the rest screen to the lock screen.
    pub fn session_key(&mut self, key: crate::idle::RestAdvance) -> bool {
        self.note_activity();
        let before = self.session;
        let (next, password_input) = self.session.on_key(key);
        self.session = next;
        if password_input {
            // On the lock screen the key types into the password field.
            if let crate::idle::RestAdvance::Character = key {
                if let crate::idle::Session::Lock = self.session {
                    self.password.push('x');
                }
            }
        }
        before != self.session
    }

    /// An upward drag on the rest screen moves to the lock screen.
    pub fn session_drag(&mut self, dy: f32) -> bool {
        let before = self.session;
        self.session = self.session.on_drag(dy);
        self.session != before
    }

    /// Whether rendering should pause (the output is blank).
    pub fn output_blank(&self) -> bool {
        self.session == crate::idle::Session::Blank
    }

    /// Mark compositor-owned surfaces dirty without asking a backend to
    /// redraw immediately. The backend decides when the next present boundary
    /// is safe, so this works for both Winit and DRM/KMS.
    pub(crate) fn request_redraw(&mut self) {
        self.redraw_pending = true;
    }

    /// Consume a dirty notification at the beginning of a frame.
    pub(crate) fn consume_redraw_request(&mut self) {
        self.redraw_pending = false;
    }

    /// Whether the backend should schedule another present.
    pub(crate) fn redraw_needed(&self) -> bool {
        if self.redraw_pending || !self.animations.is_empty() {
            return true;
        }

        let welcome_animating = match self.welcome {
            Some(crate::welcome::WelcomeStage::FirstRun) => {
                !crate::welcome::WelcomeFrame::elapsed(self.welcome_started.elapsed()).finished
            }
            Some(crate::welcome::WelcomeStage::Update) => {
                !crate::welcome::UpdateFrame::elapsed(self.welcome_started.elapsed()).finished
            }
            Some(crate::welcome::WelcomeStage::Desktop) => false,
            None => false,
        };
        let dock_animating = self
            .launch_bounces
            .values()
            .any(|started| started.elapsed().as_secs_f32() < 0.7);
        welcome_animating || dock_animating
    }

    /// The lock screen's password field.
    pub fn password_type(&mut self, text: &str) {
        for character in text.chars() {
            self.password.push(character);
        }
    }

    pub fn password_backspace(&mut self) {
        self.password.backspace();
    }

    /// Verify the typed password and unlock.
    pub fn password_submit(&mut self) -> crate::idle::UnlockResult {
        let typed = self.password.take();
        match verify_password(&self.profile_name(), &typed) {
            true => {
                self.session = crate::idle::Session::Active;
                self.note_activity();
                crate::idle::UnlockResult::Unlocked
            }
            false => crate::idle::UnlockResult::Wrong,
        }
    }

    /// The lock screen's account name.
    pub fn profile_name(&self) -> String {
        whoami_cached()
    }

    // ----- Built-in terminal ---------------------------------------------

    /// Toggle the compositor-owned terminal window. A first open allocates a
    /// visual tab and immediately starts its real PTY session through the
    /// public terminal core; this UI layer never fakes command output.
    pub fn toggle_terminal(&mut self) {
        let session_request = self.terminal_ui.toggle();
        if self.terminal_ui.open {
            self.app_store_open = false;
            self.notifications_open = false;
            self.launcher_open = false;
            self.finder_open = false;
            self.settings_open = false;
            self.control_open = false;
            if let Some(tab_id) = session_request {
                self.start_terminal_session(tab_id);
            }
        }
        self.request_redraw();
    }

    /// Open a PTY using dimensions derived from the rendered content area.
    fn start_terminal_session(&mut self, tab_id: u64) {
        let (columns, rows) = self.terminal_dimensions();
        let config = match crate::terminal::TerminalConfig::new(columns, rows) {
            Ok(config) => config,
            Err(error) => {
                warn!(?error, "Could not build terminal session configuration");
                self.terminal_ui
                    .mark_session_exited(tab_id, Some("Terminal unavailable"));
                return;
            }
        };

        match self.terminal.open_with(config) {
            Ok(session_id) => {
                self.terminal_session_map.insert(tab_id, session_id);
                self.dock.window_mapped("rouch-terminal", "Terminal");
                self.terminal.select(session_id);
                info!(tab_id, session_id, "Started built-in terminal PTY");
                let _ = self.poll_terminal();
            }
            Err(error) => {
                warn!(?error, "Could not start built-in terminal PTY");
                self.terminal_ui
                    .mark_session_exited(tab_id, Some("Terminal unavailable"));
            }
        }
    }

    fn terminal_dimensions(&self) -> (u16, u16) {
        let content = self.terminal_ui.layout(self.windows.work_area()).content;
        let columns = (content.size.width / 8).clamp(40, crate::terminal::MAX_COLUMNS as i32) as u16;
        let rows = (content.size.height / 20).clamp(12, crate::terminal::MAX_ROWS as i32) as u16;
        (columns, rows)
    }

    /// Poll non-blocking PTY readers and copy only changed snapshots into the
    /// bounded visual projection. Returns true when a redraw is useful.
    pub fn poll_terminal(&mut self) -> bool {
        let bytes = self.terminal.poll();
        let mut snapshots = Vec::new();

        for (&tab_id, &session_id) in &self.terminal_session_map {
            let Some(session) = self.terminal.session(session_id) else {
                continue;
            };
            let exited = !session.is_running();
            let visual_alive = self
                .terminal_ui
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .is_some_and(|tab| tab.alive);
            if bytes == 0 && !(exited && visual_alive) {
                continue;
            }

            let cursor = session.screen().cursor();
            snapshots.push((
                tab_id,
                session.title().to_owned(),
                session.cwd().to_string_lossy().into_owned(),
                session.screen().line_texts(),
                cursor.row,
                cursor.column,
                cursor.visible,
                !exited,
            ));
        }

        let mut changed = bytes > 0;
        for (tab_id, title, cwd, lines, row, column, cursor_visible, running) in snapshots {
            changed |= self.terminal_ui.set_snapshot(
                tab_id,
                Some(&title),
                Some(&cwd),
                &lines,
                row,
                column,
                cursor_visible,
            );
            if !running {
                changed |= self.terminal_ui.mark_session_exited(tab_id, Some(&title));
            }
        }
        changed
    }

    pub fn terminal_new_tab(&mut self) -> bool {
        let Some(tab_id) = self.terminal_ui.new_tab() else {
            return false;
        };
        self.start_terminal_session(tab_id);
        self.request_redraw();
        true
    }

    pub fn terminal_close_tab(&mut self, index: usize) -> bool {
        let Some(tab_id) = self.terminal_ui.tabs.get(index).map(|tab| tab.id) else {
            return false;
        };
        let Some(session_id) = self.terminal_session_map.remove(&tab_id) else {
            let _ = self.terminal_ui.close_tab(index);
            self.request_redraw();
            return true;
        };
        let closed = self.terminal_ui.close_tab(index).is_some();
        if closed {
            self.terminal.close(session_id);
            if self.terminal_ui.tabs.is_empty() {
                self.dock.window_unmapped("rouch-terminal", 0);
            }
            if let Some(active_id) = self.terminal_ui.active_tab().map(|tab| tab.id) {
                if let Some(active_session) = self.terminal_session_map.get(&active_id).copied() {
                    self.terminal.select(active_session);
                }
            }
            self.request_redraw();
        }
        closed
    }

    pub fn terminal_select_tab(&mut self, index: usize) -> bool {
        if !self.terminal_ui.select_tab(index) {
            return false;
        }
        if let Some(tab_id) = self.terminal_ui.active_tab().map(|tab| tab.id) {
            if let Some(session_id) = self.terminal_session_map.get(&tab_id).copied() {
                self.terminal.select(session_id);
            }
        }
        self.request_redraw();
        true
    }

    pub fn terminal_begin_title_edit(&mut self) -> bool {
        self.terminal_ui.begin_title_edit()
    }

    pub fn terminal_title_type(&mut self, text: &str) {
        self.terminal_ui.title_type(text);
    }

    pub fn terminal_title_backspace(&mut self) {
        self.terminal_ui.title_backspace();
    }

    pub fn terminal_cancel_title_edit(&mut self) {
        self.terminal_ui.cancel_title_edit();
    }

    pub fn terminal_commit_title(&mut self) -> bool {
        let changed = self.terminal_ui.commit_title();
        if let Some(tab) = self.terminal_ui.active_tab() {
            if let Some(session_id) = self.terminal_session_map.get(&tab.id).copied() {
                if let Some(session) = self.terminal.session_mut(session_id) {
                    if let Err(error) = session.set_title(&tab.title) {
                        warn!(?error, "Could not apply terminal tab title");
                    }
                }
            }
        }
        changed
    }

    pub fn terminal_cycle_theme(&mut self) -> bool {
        self.terminal_ui.cycle_theme()
    }

    pub fn terminal_toggle_transparency(&mut self) -> bool {
        self.terminal_ui.toggle_tab_transparency()
    }

    pub fn terminal_toggle_blur(&mut self) -> bool {
        self.terminal_ui.toggle_tab_blur()
    }

    /// Send already encoded keyboard bytes to the active real PTY session.
    pub fn terminal_send_bytes(&mut self, bytes: &[u8]) -> bool {
        let Some(tab_id) = self.terminal_ui.active_tab().map(|tab| tab.id) else {
            return false;
        };
        let Some(session_id) = self.terminal_session_map.get(&tab_id).copied() else {
            return false;
        };
        let sent = self
            .terminal
            .session_mut(session_id)
            .and_then(|session| session.send_bytes(bytes).ok())
            .is_some();
        if sent {
            self.request_redraw();
        }
        sent
    }

    pub fn terminal_send_text(&mut self, text: &str) -> bool {
        self.terminal_send_bytes(text.as_bytes())
    }

    // ----- Desktop icons ---------------------------------------------------

    /// Activate a desktop icon: folders open the Finder at them.
    pub fn desktop_activate(&mut self, index: usize) -> bool {
        let Some(icon) = self.desktop.icons.get(index).cloned() else {
            return false;
        };
        match icon.target {
            crate::desktop_icons::DesktopTarget::Path { path, directory } => {
                let base = crate::user::desktop_dir();
                let full = base.join(&path);
                if directory {
                    let target = full.to_string_lossy().into_owned();
                    self.finder_open = true;
                    self.finder_navigate(&target);
                    true
                } else {
                    finder_backend::open_path(&full);
                    true
                }
            }
            crate::desktop_icons::DesktopTarget::Application { app_id } => {
                self.launch(&app_id);
                true
            }
        }
    }

    fn init_wayland_listener(
        display: Display<Self>,
        event_loop: &mut EventLoop<Self>,
    ) -> Result<OsString, Box<dyn std::error::Error>> {
        let listening_socket = ListeningSocketSource::new_auto()?;
        let socket_name = listening_socket.socket_name().to_os_string();
        let loop_handle = event_loop.handle();

        loop_handle.insert_source(listening_socket, |client_stream, _, state| {
            match state
                .display_handle
                .insert_client(client_stream, Arc::new(RouchClientState::default()))
            {
                Ok(_) => {
                    state.connected_clients += 1;
                    info!(clients = state.connected_clients, "Wayland client connected");
                }
                Err(error) => warn!(?error, "Could not accept Wayland client"),
            }
        })?;

        // The display source drives client requests without a busy loop, which
        // matters on integrated GPUs and lower-power CPUs.
        loop_handle.insert_source(
            Generic::new(display, Interest::READ, LoopMode::Level),
            |_, display, state| {
                // The generic source owns `display` for the entire event loop;
                // Calloop gives this callback mutable access only while it is live.
                let dispatch_result = unsafe { display.get_mut().dispatch_clients(state) };
                if let Err(error) = dispatch_result {
                    warn!(?error, "Wayland client dispatch failed");
                }
                if let Err(error) = state.display_handle.flush_clients() {
                    warn!(?error, "Could not flush Wayland clients");
                }
                Ok(PostAction::Continue)
            },
        )?;

        Ok(socket_name)
    }

    /// Hand the session environment to systemd and D-Bus.
    ///
    /// Portals, notification daemons and every other D-Bus activated service
    /// are started by the user bus, not by this process, so they never inherit
    /// the compositor's environment. Without this step `xdg-desktop-portal`
    /// comes up with no `WAYLAND_DISPLAY` and no `XDG_CURRENT_DESKTOP`, which
    /// breaks file dialogs, screenshots, screen sharing and "open link" for
    /// every sandboxed application.
    ///
    /// Only the native session may do this: a nested session would overwrite
    /// the host desktop's own variables.
    pub(crate) fn export_session_environment(&self) {
        const VARIABLES: [&str; 4] = [
            "WAYLAND_DISPLAY",
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_TYPE",
            "XDG_SESSION_DESKTOP",
        ];

        // SAFETY: single-threaded startup, before any client can connect.
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", &self.socket_name);
            std::env::set_var("XDG_CURRENT_DESKTOP", CURRENT_DESKTOP);
            std::env::set_var("XDG_SESSION_DESKTOP", CURRENT_DESKTOP);
            std::env::set_var("XDG_SESSION_TYPE", "wayland");
        }

        for (program, leading) in [
            ("systemctl", vec!["--user", "import-environment"]),
            ("dbus-update-activation-environment", vec!["--systemd"]),
        ] {
            let mut command = std::process::Command::new(program);
            command
                .args(&leading)
                .args(VARIABLES)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            match command.status() {
                Ok(status) if status.success() => {
                    info!(program, "Exported the session environment");
                }
                Ok(status) => warn!(program, ?status, "Session environment export was rejected"),
                Err(error) => warn!(program, ?error, "Session environment export is unavailable"),
            }
        }
    }

    /// Publish the `zwp_linux_dmabuf_v1` global for the renderer's formats.
    ///
    /// Backends call this once their renderer exists. Until the global is up,
    /// Mesa's Wayland EGL platform refuses to initialize, so every accelerated
    /// client either renders through software or fails to start.
    pub(crate) fn enable_dmabuf(&mut self, formats: smithay::backend::allocator::format::FormatSet) {
        if self.dmabuf_global.is_some() {
            return;
        }
        let count = formats.iter().count();
        if count == 0 {
            warn!("Renderer reported no dmabuf formats; accelerated clients will use wl_shm");
            return;
        }
        self.dmabuf_global = Some(self.dmabuf_state.create_global::<Self>(&self.display_handle, formats));
        info!(formats = count, "Published the dmabuf global for accelerated clients");
    }

    fn window_id(&self, surface: &ToplevelSurface) -> Option<WindowId> {
        self.xdg_windows
            .iter()
            .find(|window| window.surface == *surface)
            .map(|window| window.id)
    }

    fn surface_for_window(&self, id: WindowId) -> Option<ToplevelSurface> {
        self.xdg_windows
            .iter()
            .find(|window| window.id == id)
            .map(|window| window.surface.clone())
    }

    /// Synchronize compositor-owned window state to the XDG protocol. This is
    /// the one gateway for state changes, keeping dock/menu/shortcut actions
    /// and client requests visually consistent once rendering is installed.
    fn reconfigure_windows(&mut self) {
        self.sync_space();
        let bounds = self.windows.work_area().size;
        let pending: Vec<(ToplevelSurface, Window)> = self
            .xdg_windows
            .iter()
            .filter_map(|binding| {
                self.windows
                    .window(binding.id)
                    .cloned()
                    .map(|window| (binding.surface.clone(), window))
            })
            .collect();

        for (surface, window) in pending {
            configure_toplevel(&surface, &window, bounds);
        }
    }

    /// Mirror the compositor's authoritative window stack into Smithay's
    /// render space. `WindowManager` gives us the macOS-style z order; `Space`
    /// handles output enter/leave and surface-tree rendering.
    fn sync_space(&mut self) {
        let stack = self
            .windows
            .windows()
            .iter()
            .map(|window| {
                (
                    window.id(),
                    window.frame(),
                    window.active(),
                    window.visible_on(self.windows.current_workspace()),
                )
            })
            .collect::<Vec<_>>();

        for (id, frame, active, visible) in stack {
            let Some(desktop) = self
                .xdg_windows
                .iter()
                .find(|window| window.id == id)
                .map(|window| window.desktop.clone())
            else {
                continue;
            };

            if visible {
                self.space
                    .map_element(desktop, (frame.origin.x, frame.origin.y), active);
            } else {
                self.space.unmap_elem(&desktop);
            }
        }
    }

    pub(super) fn update_output_size(&mut self, size: Size<i32, Physical>) {
        let mode = Mode {
            size,
            refresh: 60_000,
        };
        self.output.change_current_state(Some(mode), None, None, None);
        self.output.set_preferred(mode);
        self.windows.set_work_area(work_area_for(size));
        self.reconfigure_windows();
    }

    fn minimize_toplevel(&mut self, id: WindowId) {
        if self.windows.minimize(id) {
            self.start_animation(id, crate::anim::Transition::Minimize);
            self.sync_dock_minimized(id);
            self.reconfigure_windows();
        }
    }

    /// Un-minimize a window with the Liquid Material restore animation.
    pub fn restore_toplevel(&mut self, id: WindowId) {
        if self.windows.restore(id) {
            self.start_animation(id, crate::anim::Transition::Restore);
            self.sync_dock_minimized(id);
            self.reconfigure_windows();
        }
    }

    /// Refresh the dock's minimized badge count for a window's application.
    fn sync_dock_minimized(&mut self, id: WindowId) {
        if let Some(app_id) = self.window_apps.get(&id).cloned() {
            let count = self
                .window_apps
                .iter()
                .filter(|(win, app)| {
                    **app == app_id
                        && self
                            .windows
                            .window(**win)
                            .is_some_and(|window| window.minimized())
                })
                .count();
            self.dock.set_minimized(&app_id, count);
        }
    }

    fn maximize_toplevel(&mut self, id: WindowId) {
        if self.windows.maximize(id) {
            self.reconfigure_windows();
        }
    }

    fn unmaximize_toplevel(&mut self, id: WindowId) {
        if self.windows.unmaximize(id) {
            self.reconfigure_windows();
        }
    }

    fn fullscreen_toplevel(&mut self, id: WindowId) {
        if self.windows.enter_fullscreen(id) {
            self.reconfigure_windows();
        }
    }

    fn unfullscreen_toplevel(&mut self, id: WindowId) {
        if self.windows.exit_fullscreen(id) {
            self.reconfigure_windows();
        }
    }

    /// This will be invoked by the dock/window controls once they exist. XDG
    /// requires that the client, not the compositor, destroys its surface.
    #[allow(dead_code)]
    fn close_toplevel(&mut self, id: WindowId) {
        if self.windows.request_close(id) {
            if let Some(surface) = self.surface_for_window(id) {
                surface.send_close();
            }
            self.reconfigure_windows();
        }
    }

    /// Begin an interactive move or resize from a chrome action, using the
    /// pointer serial of the press that chose the action.
    pub fn begin_chrome_action(
        &mut self,
        id: WindowId,
        action: ChromeAction,
        press: (f64, f64),
        serial: smithay::utils::Serial,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let press = smithay::utils::Point::<f64, smithay::utils::Logical>::from(press);
        self.last_grab_serial = Some(serial);

        match action {
            ChromeAction::TrafficLight(TrafficLightKind::Close) => {
                self.close_toplevel(id);
            }
            ChromeAction::TrafficLight(TrafficLightKind::Minimize) => {
                self.minimize_toplevel(id);
            }
            ChromeAction::TrafficLight(TrafficLightKind::Maximize) => {
                // The green light is fullscreen on modern macOS; Super+W is
                // the plain maximize toggle.
                if self.windows.toggle_fullscreen(id) {
                    self.reconfigure_windows();
                }
            }
            ChromeAction::DragTitleBar => {
                self.active_drag = Some(id);
                grabs::start_move(self, &pointer, serial, id, press);
            }
            ChromeAction::Resize(edge) => {
                self.active_drag = Some(id);
                grabs::start_resize(self, &pointer, serial, id, edge, press);
            }
            ChromeAction::Client => {}
        }
    }

    /// Start a Liquid Material transition for one window. Only the newest
    /// transition for a window runs; a restore mid-minimize replaces it.
    pub fn start_animation(&mut self, id: WindowId, transition: crate::anim::Transition) {
        self.animations.insert(id, (transition, Instant::now()));
        self.request_redraw();
    }

    /// Read the remembered version and classify this session's welcome.
    fn detect_welcome_stage() -> Option<crate::welcome::WelcomeStage> {
        use crate::welcome::WelcomeStage;

        let installed = std::fs::read_to_string(state_path())
            .ok()
            .map(|raw| raw.trim().to_owned());
        let stage = WelcomeStage::decide(installed.as_deref(), env!("CARGO_PKG_VERSION"));
        if stage != WelcomeStage::FirstRun {
            // Never replay the first-run film, even after a state wipe that
            // forgets the version: only true updates re-show the card.
            if std::fs::write(state_path(), env!("CARGO_PKG_VERSION")).is_err() {
                warn!("Could not persist the running version; welcome may replay");
            }
        }
        match stage {
            crate::welcome::WelcomeStage::Desktop => None,
            other => Some(other),
        }
    }

    /// Mark the welcome experience as finished and remember the version.
    pub fn dismiss_welcome(&mut self) {
        self.welcome = None;
        self.request_redraw();
        if std::fs::write(state_path(), env!("CARGO_PKG_VERSION")).is_err() {
            warn!("Could not persist the running version");
        }
    }

    /// The update card's geometry hit against the dismiss button.
    pub fn welcome_dismissed_by(
        &mut self,
        position: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> bool {
        let work_area = self.windows.work_area();
        let layout = crate::welcome::update_layout(work_area);
        if crate::welcome::update_button_hit(
            &layout,
            crate::windowing::Point {
                x: position.x.round() as i32,
                y: position.y.round() as i32,
            },
        ) {
            self.dismiss_welcome();
            true
        } else {
            false
        }
    }

    /// A press on a dock icon: focus the app's windows, restore its
    /// minimized ones, or launch it when nothing is running.
    pub fn activate_dock_item(&mut self, index: usize) {
        let Some(item) = self.dock.items().get(index).cloned() else {
            return;
        };

        // Shell-owned dock slots.
        match item.app_id.as_str() {
            "rouch-terminal" => {
                self.toggle_terminal();
                return;
            }
            "launcher" => {
                self.toggle_launcher();
                return;
            }
            "app-store" => {
                self.toggle_app_store();
                return;
            }
            "settings" => {
                self.toggle_settings();
                return;
            }
            "org.gnome.Files" if self.applications.iter().all(|app| app.id != "org.gnome.Files") => {
                // No desktop entry for Files on this host: open the Finder.
                self.toggle_finder();
                return;
            }
            _ => {}
        }

        let own_windows: Vec<WindowId> = self
            .window_apps
            .iter()
            .filter(|(_, app)| **app == item.app_id)
            .map(|(id, _)| *id)
            .collect();
        let minimized: Vec<WindowId> = own_windows
            .iter()
            .copied()
            .filter(|id| self.windows.window(*id).is_some_and(|w| w.minimized()))
            .collect();

        if !minimized.is_empty() {
            for id in minimized {
                self.restore_toplevel(id);
            }
            if let Some(first) = own_windows
                .iter()
                .copied()
                .find(|id| self.windows.window(*id).is_some_and(|w| !w.minimized()))
            {
                let _ = self.windows.focus(first);
                self.reconfigure_windows();
            }
            return;
        }

        if let Some(first) = own_windows
            .iter()
            .copied()
            .find(|id| self.windows.window(*id).is_some_and(|w| !w.minimized()))
        {
            let _ = self.windows.focus(first);
            self.reconfigure_windows();
            return;
        }

        // Nothing running: bounce the icon and launch the application
        // through the desktop database, falling back to the old guesses.
        self.launch_bounces.insert(item.app_id.clone(), Instant::now());
        info!(app_id = %item.app_id, "Launching application from the dock");
        if self.applications.iter().any(|app| app.id == item.app_id) {
            self.launch(&item.app_id);
        } else {
            self.spawn_app(&item.app_id);
        }
    }

    /// Launch an application by desktop id through the session launcher.
    /// Open a dock item.
    ///
    /// Dock ids are desktop-entry ids, so the scanned application database is
    /// the authority. The alias table below only covers the handful of default
    /// dock entries whose id does not match any installed entry, so a fresh
    /// install still opens something instead of logging and doing nothing.
    fn spawn_app(&mut self, app_id: &str) {
        if self.applications.iter().any(|app| app.id == app_id) {
            self.launch(app_id);
            return;
        }

        // A Flatpak of the same application is exported under the same
        // reverse-DNS id, so a case-insensitive match still finds it.
        let matched = self
            .applications
            .iter()
            .find(|app| app.id.eq_ignore_ascii_case(app_id))
            .map(|app| app.id.clone());
        if let Some(id) = matched {
            self.launch(&id);
            return;
        }

        let fallback = match app_id {
            "weston-terminal" => Some("weston-terminal"),
            "org.gnome.Files" => Some("nautilus"),
            "org.mozilla.firefox" => Some("firefox"),
            "org.gnome.TextEditor" => Some("gnome-text-editor"),
            "org.gnome.Calculator" => Some("gnome-calculator"),
            "org.gnome.Settings" => Some("gnome-control-center"),
            _ => None,
        };
        let Some(program) = fallback else {
            warn!(app_id, "No installed desktop entry matches this dock item");
            return;
        };

        let mut command = std::process::Command::new(program);
        self.prepare_client_command(&mut command);
        if let Err(error) = command.spawn() {
            warn!(program, ?error, "Could not launch application");
        }
    }

    /// Rescan the desktop-entry database.
    ///
    /// Installing an application must not require a new session, so the
    /// launcher refreshes on open. The scan is paced because it touches every
    /// entry in every XDG data directory.
    pub fn refresh_applications(&mut self) {
        const RESCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

        if self
            .applications_scanned_at
            .is_some_and(|at| at.elapsed() < RESCAN_INTERVAL)
        {
            return;
        }
        self.applications = scan_applications();
        self.applications_scanned_at = Some(Instant::now());
    }

    /// The launch-bounce offset for one dock item this frame.
    pub fn dock_bounce(&self, index: usize) -> f32 {
        let Some(item) = self.dock.items().get(index) else {
            return 0.0;
        };
        let Some(started) = self.launch_bounces.get(&item.app_id) else {
            return 0.0;
        };

        // One bounce cycle per launch, ~0.7s, until the window maps.
        let elapsed = started.elapsed().as_secs_f32();
        const CYCLE: f32 = 0.7;
        if elapsed >= CYCLE {
            return 0.0;
        }
        crate::dock::bounce_offset(elapsed / CYCLE)
    }

    /// Drop finished transitions. The renderer asks `animation_sample` for
    /// live values, so cleanup here is all that is needed per frame.
    pub fn refresh_animations(&mut self) {
        self.animations.retain(|_, (transition, started)| {
            !crate::anim::Progress::elapsed(*transition, started.elapsed()).finished()
        });
        self.launch_bounces
            .retain(|_, started| started.elapsed().as_secs_f32() < 0.7);
    }

    /// The animated scale, opacity and animated centre for a window this
    /// frame, if any. `None` means draw the window normally.
    pub fn animation_sample(&self, id: WindowId) -> Option<decorations::AnimationSample> {
        let (transition, started) = self.animations.get(&id)?;
        let progress = crate::anim::Progress::elapsed(*transition, started.elapsed());
        let scale = crate::anim::interpolated_scale(*transition, progress);
        let opacity = crate::anim::interpolated_opacity(*transition, progress);

        // Minimize shrinks toward the bottom-centre dock strip; restore
        // mirrors it back. Open and close stay anchored at the window.
        let work_area = self.windows.work_area();
        let target_x = match transition.target() {
            crate::anim::Target::DockStrip => work_area.origin.x as f64 + work_area.size.width as f64 / 2.0,
            crate::anim::Target::WindowFrame => self
                .windows
                .window(id)
                .map(|w| w.frame().origin.x as f64 + w.frame().size.width as f64 / 2.0)?,
        };
        let target_y = match transition.target() {
            crate::anim::Target::DockStrip => work_area.bottom() as f64 - 40.0,
            crate::anim::Target::WindowFrame => self
                .windows
                .window(id)
                .map(|w| w.frame().origin.y as f64 + w.frame().size.height as f64 / 2.0)?,
        };

        let window = self.windows.window(id)?;
        let origin_x = window.frame().origin.x as f64 + window.frame().size.width as f64 / 2.0;
        let origin_y = window.frame().origin.y as f64 + window.frame().size.height as f64 / 2.0;

        let t = progress.value;
        // Minimize: window centre slides toward the dock target as it scales
        // down. Restore plays the same path backwards.
        let centre = smithay::utils::Point::<f64, smithay::utils::Logical>::from((
            origin_x + (target_x - origin_x) * t as f64,
            origin_y + (target_y - origin_y) * t as f64,
        ));

        Some((scale, opacity, centre))
    }
}

impl DragTarget for Rouch {
    fn drag_frame(&self, id: WindowId) -> Option<Rect> {
        self.windows.window(id).map(Window::frame)
    }

    fn drag_move_to(&mut self, id: WindowId, position: RouchPoint) {
        self.windows.move_to(id, position);
    }

    fn drag_resize_by(&mut self, id: WindowId, edge: ResizeEdge, delta: RouchPoint) {
        self.windows.resize_by(id, edge, delta);
    }

    fn drag_reconfigured(&mut self) {
        self.reconfigure_windows();
    }
}

fn work_area_for(size: Size<i32, Physical>) -> Rect {
    Rect::new(0, 0, size.w, size.h)
}

fn surface_app_id(surface: &ToplevelSurface) -> Option<String> {
    let app_id = smithay::wayland::compositor::with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok()?.app_id.clone())
    })?;
    (!app_id.is_empty()).then_some(app_id)
}

fn surface_client_pid(handle: &DisplayHandle, surface: &ToplevelSurface) -> Option<u32> {
    let client = surface.wl_surface().client()?;
    let credentials = client.get_credentials(handle).ok()?;
    let pid = credentials.pid as u32;
    (pid > 0).then_some(pid)
}

fn configure_toplevel(surface: &ToplevelSurface, window: &Window, bounds: WindowSize) {
    // XDG requires the initial configure to leave the size unset so the client
    // maps at its own natural geometry. Forcing one opens every dialog and
    // utility window at the compositor's placeholder size.
    let initial = !surface.is_initial_configure_sent();

    surface.with_pending_state(|state| {
        state.size = (!initial).then(|| {
            Size::<i32, Logical>::from((window.frame().size.width, window.frame().size.height))
        });
        state.bounds = Some(Size::<i32, Logical>::from((bounds.width, bounds.height)));

        set_xdg_state(
            &mut state.states,
            xdg_toplevel::State::Activated,
            window.active() && !window.minimized(),
        );
        set_xdg_state(
            &mut state.states,
            xdg_toplevel::State::Maximized,
            window.maximized(),
        );
        set_xdg_state(
            &mut state.states,
            xdg_toplevel::State::Fullscreen,
            window.fullscreen(),
        );
    });

    // An XDG configure is required for the initial handshake and after every
    // compositor decision that changes frame or state.
    let _serial = surface.send_configure();
}

fn set_xdg_state(
    states: &mut smithay::wayland::shell::xdg::ToplevelStateSet,
    state: xdg_toplevel::State,
    enabled: bool,
) {
    if enabled {
        states.set(state);
    } else {
        states.unset(state);
    }
}

impl CompositorHandler for Rouch {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<RouchClientState>()
            .expect("Rouch clients always carry compositor state")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.request_redraw();
        self.committed_surfaces += 1;
        trace!(
            uptime_ms = self.started_at.elapsed().as_millis(),
            commits = self.committed_surfaces,
            "Wayland surface committed"
        );

        let Some(index) = self
            .xdg_windows
            .iter()
            .position(|window| window.surface.wl_surface() == surface)
        else {
            return;
        };
        // This checks the XDG initial-configure contract before a renderer
        // ever reads a client buffer.
        if !self.xdg_windows[index].surface.ensure_configured() {
            return;
        }
        if self.xdg_windows[index].adopted_size {
            return;
        }

        // The client has now mapped at the size it chose. Adopt it once so the
        // compositor's pre-map placeholder does not become every window's size.
        let client_size = self.xdg_windows[index].desktop.geometry().size;
        if client_size.w <= 0 || client_size.h <= 0 {
            return;
        }
        self.xdg_windows[index].adopted_size = true;

        let id = self.xdg_windows[index].id;
        let Some(current) = self.windows.window(id).map(|window| window.frame().size) else {
            return;
        };
        let delta = RouchPoint::new(
            client_size.w - current.width,
            client_size.h - current.height,
        );
        if delta.x == 0 && delta.y == 0 {
            return;
        }
        self.windows.resize_by(id, ResizeEdge::BottomRight, delta);
        self.reconfigure_windows();
    }
}

impl XdgShellHandler for Rouch {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_client(&mut self, _client: ShellClient) {
        debug!("XDG client created");
    }

    fn client_pong(&mut self, _client: ShellClient) {
        trace!("XDG client responded to liveness ping");
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let id = self
            .windows
            .create_window(WindowSize::new(960, 640), SizeLimits::default());
        let desktop = DesktopWindow::new_wayland_window(surface.clone());
        self.xdg_windows.push(XdgWindow {
            id,
            surface,
            desktop,
            adopted_size: false,
        });
        self.start_animation(id, crate::anim::Transition::Open);
        self.request_redraw();
        info!(window_id = id.raw(), "XDG toplevel created");
        self.reconfigure_windows();
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let geometry = positioner.get_geometry();
        surface.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });

        if let Err(error) = surface.send_configure() {
            warn!(?error, "Could not configure XDG popup");
            return;
        }
        self.popups.push(surface);
        self.request_redraw();
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, serial: Serial) {
        let Some(id) = self.window_id(&surface) else {
            return;
        };
        // XDG requires the serial to identify an explicit user press. Only a
        // serial this compositor emitted is accepted; a client replaying an
        // old or guessed serial cannot move someone else's window.
        if !self.pressed_serials.validates(serial) {
            debug!(
                window_id = id.raw(),
                ?serial,
                "Rejecting XDG move request: unvalidated serial"
            );
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let press = pointer.current_location();
        self.last_grab_serial = Some(serial);
        self.active_drag = Some(id);
        grabs::start_move(self, &pointer, serial, id, press);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edge: xdg_toplevel::ResizeEdge,
    ) {
        let Some(id) = self.window_id(&surface) else {
            return;
        };
        if !self.pressed_serials.validates(serial) {
            debug!(
                window_id = id.raw(),
                ?serial,
                "Rejecting XDG resize request: unvalidated serial"
            );
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let edge = match edge {
            xdg_toplevel::ResizeEdge::Top => ResizeEdge::Top,
            xdg_toplevel::ResizeEdge::Bottom => ResizeEdge::Bottom,
            xdg_toplevel::ResizeEdge::Left => ResizeEdge::Left,
            xdg_toplevel::ResizeEdge::Right => ResizeEdge::Right,
            xdg_toplevel::ResizeEdge::TopLeft => ResizeEdge::TopLeft,
            xdg_toplevel::ResizeEdge::TopRight => ResizeEdge::TopRight,
            xdg_toplevel::ResizeEdge::BottomLeft => ResizeEdge::BottomLeft,
            xdg_toplevel::ResizeEdge::BottomRight => ResizeEdge::BottomRight,
            xdg_toplevel::ResizeEdge::None => return,
            _ => return,
        };
        let press = pointer.current_location();
        self.last_grab_serial = Some(serial);
        self.active_drag = Some(id);
        grabs::start_resize(self, &pointer, serial, id, edge, press);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, serial: Serial) {
        // Popup grabs become active together with the pointer backend. Keeping
        // the request explicit avoids treating an unverified serial as input.
        debug!(?serial, "XDG popup grab requested");
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(&surface) {
            self.maximize_toplevel(id);
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(&surface) {
            self.unmaximize_toplevel(id);
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        if let Some(id) = self.window_id(&surface) {
            self.fullscreen_toplevel(id);
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(&surface) {
            self.unfullscreen_toplevel(id);
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(&surface) {
            self.minimize_toplevel(id);
        }
    }

    fn show_window_menu(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _location: smithay::utils::Point<i32, Logical>,
    ) {
        if let Some(id) = self.window_id(&surface) {
            if self.windows.focus(id) {
                self.reconfigure_windows();
            }
            // Rendering the Liquid Material window menu belongs to the shell
            // UI milestone; focusing first is the protocol-side behavior.
            debug!(window_id = id.raw(), "XDG window menu requested");
        }
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        trace!(?configure, surface = ?surface.id(), "XDG configure acknowledged");
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        let geometry = positioner.get_geometry();
        surface.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
        if let Err(error) = surface.send_configure() {
            warn!(?error, "Could not reposition XDG popup");
        }
    }

    fn client_destroyed(&mut self, _client: ShellClient) {
        debug!("XDG client destroyed");
        self.request_redraw();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_id(&surface) else {
            return;
        };

        if let Some(index) = self.xdg_windows.iter().position(|window| window.id == id) {
            let window = self.xdg_windows.remove(index);
            self.space.unmap_elem(&window.desktop);
        }
        self.decorations.forget(id);

        // Update the dock: drop the running state when the last window of
        // the application is gone.
        if let Some(app_id) = self.window_apps.remove(&id) {
            let remaining = self.window_apps.values().filter(|app| **app == app_id).count();
            self.dock.window_unmapped(&app_id, remaining);
        }

        self.windows.remove(id);
        self.reconfigure_windows();
        info!(window_id = id.raw(), "XDG toplevel destroyed");
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.popups.retain(|popup| popup != &surface);
        debug!("XDG popup destroyed");
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(&surface) {
            let app_id = smithay::wayland::compositor::with_states(surface.wl_surface(), |states| {
                states
                    .data_map
                    .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().unwrap().app_id.clone())
                    .unwrap_or_default()
            });
            if !app_id.is_empty() {
                self.window_apps.insert(id, app_id.clone());
                self.dock.window_mapped(&app_id, &app_id);
                debug!(window_id = id.raw(), app_id = %app_id, "XDG app id changed");
            }
        }
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(&surface) {
            let title = {
                let wl = surface.wl_surface();
                smithay::wayland::compositor::with_states(wl, |states| {
                    states
                        .data_map
                        .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                        .and_then(|data| data.lock().unwrap().title.clone())
                        .unwrap_or_default()
                })
            };
            if self.windows.set_title(id, &title) {
                debug!(window_id = id.raw(), title = %title, "XDG title changed");
            }
        }
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(&surface) {
            debug!(window_id = id.raw(), "XDG toplevel parent changed");
        }
    }
}

impl ShmHandler for Rouch {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl BufferHandler for Rouch {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {
        trace!("Wayland client buffer destroyed");
    }
}

impl SeatHandler for Rouch {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    /// Clipboard ownership follows keyboard focus, as the data-device protocol
    /// requires: only the focused client may read the current selection.
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| surface.client());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
    }
}

impl SelectionHandler for Rouch {
    type SelectionUserData = ();
}

// Rouch never starts a compositor-initiated drag, and the client-initiated
// path is fully handled by Smithay's grab. The default methods are correct;
// the impls exist because `DataDeviceHandler` requires them.
impl ClientDndGrabHandler for Rouch {}
impl ServerDndGrabHandler for Rouch {}

impl DataDeviceHandler for Rouch {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for Rouch {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl XdgDecorationHandler for Rouch {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // Rouch owns the title bar, traffic lights and resize borders, so a
        // client must not draw its own. Answering ServerSide before the first
        // configure keeps GTK from ever mapping a client-side header bar.
        set_server_side_decoration(&toplevel);
    }

    fn request_mode(
        &mut self,
        toplevel: ToplevelSurface,
        _mode: smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    ) {
        set_server_side_decoration(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        set_server_side_decoration(&toplevel);
    }
}

/// Pin one toplevel to server-side decorations and configure it.
fn set_server_side_decoration(toplevel: &ToplevelSurface) {
    use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;

    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(Mode::ServerSide);
    });
    if toplevel.is_initial_configure_sent() {
        let _serial = toplevel.send_configure();
    }
}

impl DmabufHandler for Rouch {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        // The advertised format list comes straight from the active renderer,
        // so a buffer that matches it is importable. The real import happens
        // when the surface tree is turned into render elements; a buffer that
        // fails there degrades to a missing frame rather than a dead client.
        if let Err(error) = notifier.successful::<Self>() {
            warn!(?error, ?dmabuf, "Could not acknowledge a client dmabuf import");
        }
    }
}

impl OutputHandler for Rouch {}

/// State tied to one connecting Wayland client. Smithay cleans it up as part
/// of the normal connection lifecycle.
#[derive(Default)]
struct RouchClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for RouchClientState {
    fn initialized(&self, client_id: ClientId) {
        debug!(?client_id, "Wayland client initialized");
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        debug!(?client_id, ?reason, "Wayland client disconnected");
    }
}

delegate_compositor!(Rouch);
delegate_output!(Rouch);
delegate_xdg_shell!(Rouch);
delegate_shm!(Rouch);
delegate_seat!(Rouch);
delegate_alpha_modifier!(Rouch);
delegate_data_device!(Rouch);
delegate_primary_selection!(Rouch);
delegate_xdg_decoration!(Rouch);
delegate_dmabuf!(Rouch);
