//! Rouch is a lightweight Wayland desktop with Liquid Glass and Material-inspired geometry.

pub mod anim;

/// Native Flatpak/Flathub application gallery state and geometry.
#[cfg(any(test, target_os = "linux"))]
pub mod app_store;

/// Pure notification queue, grouping and delivery preferences.
#[cfg(any(test, target_os = "linux"))]
pub mod notifications;

/// Linux Flatpak/Flathub backend for the application gallery. The compositor
/// loop can integrate it without coupling the pure model to process I/O.
#[cfg(any(test, target_os = "linux"))]
#[path = "linux/app_store_backend.rs"]
pub mod app_store_backend;

/// Vulkan-first renderer selection and bounded performance policy.
pub mod graphics;

/// Pure evidence-based game detection and safe resource policy.
pub mod game_mode;

/// The desktop work area: a grid of icons, macOS-aligned.
#[cfg(any(test, target_os = "linux"))]
pub mod desktop_icons;

/// Parsed freedesktop `.desktop` entries: the application database.
#[cfg(any(test, target_os = "linux"))]
pub mod desktop_entry;

/// The idle-to-rest-to-lock choreography and the lock screen.
#[cfg(any(test, target_os = "linux"))]
pub mod idle;

/// The macOS-style application launcher with search.
#[cfg(any(test, target_os = "linux"))]
pub mod launcher;

/// The Finder model: pure file-browsing state, sampled by renderers.
#[cfg(any(test, target_os = "linux"))]
pub mod finder;

/// The Rouch Settings app: panes, rows and typed values.
#[cfg(any(test, target_os = "linux"))]
pub mod settings;

/// The desktop user profile, avatar marks and lock-screen identity.
#[cfg(any(test, target_os = "linux"))]
pub mod user;

/// The macOS Control Centre panel, as pure layout and hit-testing.
#[cfg(any(test, target_os = "linux"))]
pub mod control;

/// The dock behaviour, magnification math and pinned defaults.
///
/// Everything here is pure geometry: the renderer samples it and never owns layout.
#[cfg(any(test, target_os = "linux"))]
pub mod dock;

/// The macOS-style top bar: distribution mark, status cluster, clock.
///
/// Pure layout and civil-time formatting; the renderer samples it.
#[cfg(any(test, target_os = "linux"))]
pub mod topbar;

/// macOS Tahoe-style widgets: board state, edit mode, removal popup.
#[cfg(any(test, target_os = "linux"))]
pub mod widgets;

pub mod chrome;
pub mod design;
/// Explicit nested/native session selection and XDG/VT diagnostics.
#[cfg(any(test, target_os = "linux"))]
#[path = "linux/session.rs"]
pub mod session;
/// Pure first-run installation onboarding state machine.
pub mod setup;
/// Pure swapfile sizing, inspection, and explicit command planning.
pub mod swapfile;
/// Public terminal core: real PTY sessions on Linux and an explicit unsupported
/// backend elsewhere. The visual layer owns rendering and input mapping.
pub mod terminal;
/// The first-run and update welcome experience, macOS-style.
///
/// Pure choreography and geometry: the renderer samples the timelines and
/// never owns layout. Exercised from the Windows test suite too.
#[cfg(any(test, target_os = "linux"))]
pub mod welcome;
pub mod windowing;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn init_logging() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("rouch=info"));
    tracing_subscriber::fmt().with_env_filter(filter).compact().init();
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    linux::run(&args)
}

#[cfg(not(target_os = "linux"))]
fn main() {
    let profile = design::VisualProfile::DEFAULT;

    println!("{} is a Linux Wayland compositor.", profile.product_name);
    println!("Its default visual reference is {}.", profile.default_wallpaper);
    eprintln!("Run the compositor from a supported Linux installation or physical Linux session.");
}
