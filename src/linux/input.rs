//! Native input boundary for the Rouch compositor.
//!
//! The public model in this file is deliberately independent from Smithay's
//! concrete event types.  It gives the chrome, window manager and future
//! Wayland/XWayland paths one small stream to consume, while the optional
//! [`LibinputSeatSource`] adapter translates Smithay 0.7 events at the edge.
//!
//! The normalizer is also the place where low-end policy is enforced: event
//! timestamps are monotonic, malformed floating point values are discarded,
//! key-repeat catch-up is bounded, and every seat loss or device removal can
//! cancel a grab before focus is allowed to become stale.

use std::collections::HashMap;
use std::fmt;

/// Initial delay before a held key begins repeating.
pub const DEFAULT_REPEAT_DELAY_MS: u64 = 400;

/// Default repeat cadence, close to 30 events per second.
pub const DEFAULT_REPEAT_INTERVAL_MS: u64 = 33;

/// Maximum number of synthetic repeat events emitted by one timer tick.
///
/// Capping catch-up prevents a slow machine from spending an entire frame
/// replaying an old key queue after it wakes from a compositor stall.
pub const DEFAULT_REPEAT_BURST: u8 = 4;

/// Maximum sane relative motion accepted from one libinput event.
pub const MAX_RELATIVE_DELTA: f64 = 8_192.0;

/// Linux input-event code for the primary pointer button.
pub const PRIMARY_POINTER_BUTTON: u32 = 0x110;

/// Return whether a button is allowed to activate compositor-owned chrome.
/// Secondary buttons remain client input and never trigger a shell action.
pub const fn is_primary_pointer_button(button: u32) -> bool {
    button == PRIMARY_POINTER_BUTTON
}

/// The smallest logical movement that can change a shell pixel or a hit-test
/// result.  Host backends may report the same pointer position several times
/// while the compositor is catching up; those samples still need to reach a
/// Wayland client, but they do not need a new shell frame.
pub const POINTER_VISUAL_QUANTUM: f64 = 1.0;

/// A bounded, renderer-friendly pointer position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualPointerPosition {
    /// Rounded logical x coordinate.
    pub x: i32,
    /// Rounded logical y coordinate.
    pub y: i32,
}

impl VisualPointerPosition {
    /// Convert a backend coordinate without allowing NaN or infinity to
    /// enter geometry code.  Saturation keeps malformed device data inside
    /// the integer range while preserving the fact that it is off-screen.
    pub fn from_f64(x: f64, y: f64) -> Option<Self> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }

        let quantize = |value: f64| {
            (value / POINTER_VISUAL_QUANTUM)
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        };

        Some(Self {
            x: quantize(x),
            y: quantize(y),
        })
    }
}

/// Whether a pointer motion can change a pixel-aligned shell surface.
///
/// Input delivery and redraw scheduling are intentionally separate: callers
/// should always forward the motion to the seat, and use this result only for
/// deciding whether the compositor-owned chrome needs a new frame.
pub fn pointer_visual_motion_changed(previous: (f64, f64), next: (f64, f64)) -> bool {
    match (
        VisualPointerPosition::from_f64(previous.0, previous.1),
        VisualPointerPosition::from_f64(next.0, next.1),
    ) {
        (Some(previous), Some(next)) => previous != next,
        // An invalid sample is a recovery boundary.  Redrawing once lets the
        // shell remove stale hover/pressed feedback after a bad device event.
        _ => true,
    }
}

/// Actions owned by the Caelune shell rather than by a Wayland client.
///
/// Keeping this map pure makes it possible for the native and nested input
/// paths to share the same conflict policy.  It deliberately describes
/// protocol keys and modifiers, not a list of applications or compositor
/// guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellShortcut {
    /// Toggle the built-in terminal.
    ToggleTerminal,
    /// Toggle the launcher.
    ToggleLauncher,
    /// Toggle the Flatpak gallery.
    ToggleGallery,
    /// Toggle notifications.
    ToggleNotifications,
    /// Toggle Finder.
    ToggleFinder,
    /// Toggle Settings.
    ToggleSettings,
    /// Cycle windows in MRU order. `reverse` is Shift+Tab.
    CycleWindows { reverse: bool },
    /// Move to a neighboring virtual workspace.
    SwitchWorkspace { delta: i32 },
    /// Focus an MRU window by its one-based number.
    FocusWindow { number: u8 },
    /// Close the focused window.
    CloseWindow,
    /// Minimize or restore the focused window.
    MinimizeWindow,
    /// Toggle maximize for the focused window.
    ToggleMaximize,
    /// Toggle fullscreen for the focused window.
    ToggleFullscreen,
    /// Dismiss the topmost shell overlay.
    DismissOverlay,
}

/// Resolve one pressed keysym into a shell action.
///
/// `raw` uses the XKB keysym values exposed by Smithay.  The function is
/// intentionally conservative: terminal-specific Ctrl+Shift bindings are
/// not claimed here, and plain text keys remain available to clients.
pub fn shell_shortcut(raw: u32, logo: bool, ctrl: bool, alt: bool, shift: bool) -> Option<ShellShortcut> {
    const KEY_ESCAPE: u32 = 0xff1b;
    const KEY_RETURN: u32 = 0xff0d;
    const KEY_TAB: u32 = 0xff09;
    const KEY_ISO_LEFT_TAB: u32 = 0xfe20;
    const KEY_LEFT: u32 = 0xff51;
    const KEY_RIGHT: u32 = 0xff53;

    if !logo && !ctrl && !alt && !shift && raw == KEY_ESCAPE {
        return Some(ShellShortcut::DismissOverlay);
    }

    if (logo || alt) && !ctrl && (raw == KEY_TAB || raw == KEY_ISO_LEFT_TAB) {
        return Some(ShellShortcut::CycleWindows {
            reverse: shift || raw == KEY_ISO_LEFT_TAB,
        });
    }

    if ((ctrl && alt) || (logo && ctrl)) && !shift {
        return match raw {
            KEY_LEFT => Some(ShellShortcut::SwitchWorkspace { delta: -1 }),
            KEY_RIGHT => Some(ShellShortcut::SwitchWorkspace { delta: 1 }),
            _ => None,
        };
    }

    if !logo || ctrl || alt {
        return None;
    }

    if !shift && (u32::from(b'1')..=u32::from(b'9')).contains(&raw) {
        return Some(ShellShortcut::FocusWindow {
            number: (raw - u32::from(b'0')) as u8,
        });
    }

    match raw {
        KEY_RETURN => Some(ShellShortcut::ToggleTerminal),
        value if value == u32::from(b' ') => Some(ShellShortcut::ToggleLauncher),
        value if value == u32::from(b'g') || value == u32::from(b'G') => Some(ShellShortcut::ToggleGallery),
        value if value == u32::from(b'n') || value == u32::from(b'N') => {
            Some(ShellShortcut::ToggleNotifications)
        }
        value if value == u32::from(b'e') || value == u32::from(b'E') => Some(ShellShortcut::ToggleFinder),
        value if value == u32::from(b',') => Some(ShellShortcut::ToggleSettings),
        value if value == u32::from(b'q') || value == u32::from(b'Q') => Some(ShellShortcut::CloseWindow),
        value if value == u32::from(b'm') || value == u32::from(b'M') => Some(ShellShortcut::MinimizeWindow),
        value if value == u32::from(b'w') || value == u32::from(b'W') => Some(ShellShortcut::ToggleMaximize),
        value if value == u32::from(b'f') || value == u32::from(b'F') => {
            Some(ShellShortcut::ToggleFullscreen)
        }
        _ => None,
    }
}

/// A stable device identifier supplied by libinput.
pub type DeviceId = String;

/// The broad class of a physical input device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    /// A keyboard or keyboard-like device.
    Keyboard,
    /// A relative mouse or pointing device.
    Mouse,
    /// A libinput pointer device which also exposes gesture events.
    Touchpad,
    /// An absolute touchscreen or touch-capable panel.
    Touchscreen,
    /// A tablet tool or other absolute stylus device.
    Tablet,
    /// A device that does not match a class Rouch currently consumes.
    Unknown,
}

/// Pick a device class from capability facts without touching a real device.
///
/// This is intentionally public so a future non-libinput backend can use the
/// exact same classification and so tests do not need to construct libinput
/// objects.  Touch takes precedence over gesture because some touchscreen
/// devices expose both capabilities.
pub fn classify_device_capabilities(
    keyboard: bool,
    pointer: bool,
    touch: bool,
    tablet: bool,
    gesture: bool,
) -> DeviceKind {
    if touch {
        DeviceKind::Touchscreen
    } else if tablet {
        DeviceKind::Tablet
    } else if gesture && pointer {
        DeviceKind::Touchpad
    } else if keyboard {
        DeviceKind::Keyboard
    } else if pointer {
        DeviceKind::Mouse
    } else {
        DeviceKind::Unknown
    }
}

/// Which input path is currently usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBackendChoice {
    /// Native libinput through the compositor seat.
    Libinput,
    /// The existing nested host-window adapter.
    NestedHost,
    /// No input backend is available; the shell must remain recoverable.
    Unavailable,
}

/// Select native input first, then the already-supported nested path.
pub fn select_input_backend(libinput_ready: bool, nested_ready: bool) -> InputBackendChoice {
    if libinput_ready {
        InputBackendChoice::Libinput
    } else if nested_ready {
        InputBackendChoice::NestedHost
    } else {
        InputBackendChoice::Unavailable
    }
}

/// The press/release state of a physical key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhysicalKeyState {
    /// The key went down.
    Pressed,
    /// The key went up.
    Released,
}

/// The state delivered to the compositor after key-repeat normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyDelivery {
    /// A physical press.
    Pressed,
    /// A physical release.
    Released,
    /// A synthetic repeat generated by the compositor timer.
    Repeat,
}

/// A raw-ish key sample accepted by the pure normalizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySample {
    /// Event timestamp in microseconds, with an arbitrary backend epoch.
    pub time_us: u64,
    /// XKB/Linux keycode.  Smithay's `Keycode::raw()` maps to this value.
    pub keycode: u32,
    /// Physical state from libinput.
    pub state: PhysicalKeyState,
    /// Device which generated the sample.
    pub device: DeviceId,
}

/// A normalized keyboard event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// Monotonic timestamp in microseconds.
    pub time_us: u64,
    /// XKB/Linux keycode.
    pub keycode: u32,
    /// Delivery state after repeat handling.
    pub state: KeyDelivery,
    /// Device which generated the event, or the held key's device for repeat.
    pub device: DeviceId,
}

/// The source of a pointer axis event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AxisSource {
    /// Physical wheel ticks.
    Wheel,
    /// Finger scrolling on a touchpad.
    Finger,
    /// A continuous axis such as a high-resolution touchpad stream.
    Continuous,
    /// A backend did not provide a source classification.
    Unknown,
}

/// A gesture lifecycle marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GesturePhase {
    /// Gesture recognition started.
    Begin,
    /// Gesture changed.
    Update,
    /// Gesture ended.
    End {
        /// Whether libinput marked the gesture as cancelled.
        cancelled: bool,
    },
}

/// A touch lifecycle marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TouchPhase {
    /// A finger appeared.
    Down,
    /// A finger moved.
    Motion,
    /// A finger was lifted.
    Up,
    /// The complete touch sequence was cancelled.
    Cancel,
    /// The backend committed the current touch frame.
    Frame,
}

/// Why a focus or pointer grab was invalidated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrabCancelReason {
    /// The session was paused by logind/seatd during VT switch or suspend.
    SeatPaused,
    /// The host/native input device disappeared.
    DeviceRemoved,
    /// The host window lost activation.
    HostDeactivated,
    /// The focused surface or output was removed.
    FocusLost,
    /// The output owning the grab disappeared.
    OutputRemoved,
    /// The compositor explicitly replaced the grab.
    Replaced,
    /// A client or shell action requested cancellation.
    Explicit,
}

/// A single stream consumed by the shell and compositor integration layer.
#[derive(Debug, Clone, PartialEq)]
pub enum NormalizedInputEvent {
    /// A device became available.
    DeviceAdded {
        /// libinput identifier.
        device: DeviceId,
        /// Human-readable name.
        name: String,
        /// Rouch device class.
        kind: DeviceKind,
    },
    /// A device was removed.
    DeviceRemoved {
        /// libinput identifier.
        device: DeviceId,
        /// Last known Rouch device class.
        kind: DeviceKind,
    },
    /// A keyboard press, release or normalized repeat.
    Keyboard(KeyEvent),
    /// Relative mouse/touchpad motion.
    PointerMotion {
        /// Monotonic timestamp in microseconds.
        time_us: u64,
        /// Accelerated delta in compositor logical pixels.
        dx: f64,
        /// Accelerated delta in compositor logical pixels.
        dy: f64,
        /// Unaccelerated x delta, useful for pointer constraints.
        unaccelerated_dx: f64,
        /// Unaccelerated y delta, useful for pointer constraints.
        unaccelerated_dy: f64,
        /// Device identifier.
        device: DeviceId,
    },
    /// Absolute pointer motion, generally from a touchscreen/tablet.
    PointerMotionAbsolute {
        /// Monotonic timestamp in microseconds.
        time_us: u64,
        /// Backend-space x coordinate.
        x: f64,
        /// Backend-space y coordinate.
        y: f64,
        /// Device identifier.
        device: DeviceId,
    },
    /// A pointer button transition.
    PointerButton {
        /// Monotonic timestamp in microseconds.
        time_us: u64,
        /// Linux input button code.
        button: u32,
        /// Press or release.
        pressed: bool,
        /// Device identifier.
        device: DeviceId,
    },
    /// A pointer/touchpad axis event.
    PointerAxis {
        /// Monotonic timestamp in microseconds.
        time_us: u64,
        /// Horizontal pixel/value delta.
        horizontal: f64,
        /// Vertical pixel/value delta.
        vertical: f64,
        /// Optional high-resolution wheel values.
        horizontal_v120: Option<f64>,
        /// Optional high-resolution wheel values.
        vertical_v120: Option<f64>,
        /// Physical source of the axis.
        source: AxisSource,
        /// Device identifier.
        device: DeviceId,
    },
    /// A three/four-finger swipe lifecycle event.
    GestureSwipe {
        /// Monotonic timestamp in microseconds.
        time_us: u64,
        /// Begin/update/end marker.
        phase: GesturePhase,
        /// Number of fingers reported by libinput.
        fingers: u32,
        /// X movement since the previous update.
        dx: f64,
        /// Y movement since the previous update.
        dy: f64,
        /// Device identifier.
        device: DeviceId,
    },
    /// A pinch/rotate gesture lifecycle event.
    GesturePinch {
        /// Monotonic timestamp in microseconds.
        time_us: u64,
        /// Begin/update/end marker.
        phase: GesturePhase,
        /// Number of fingers reported by libinput.
        fingers: u32,
        /// Center movement since the previous update.
        dx: f64,
        /// Center movement since the previous update.
        dy: f64,
        /// Absolute scale relative to gesture start.
        scale: f64,
        /// Rotation delta in degrees.
        rotation: f64,
        /// Device identifier.
        device: DeviceId,
    },
    /// A hold gesture lifecycle event.
    GestureHold {
        /// Monotonic timestamp in microseconds.
        time_us: u64,
        /// Begin or end marker.
        phase: GesturePhase,
        /// Number of fingers reported by libinput.
        fingers: u32,
        /// Device identifier.
        device: DeviceId,
    },
    /// A touch point transition.
    Touch {
        /// Monotonic timestamp in microseconds.
        time_us: u64,
        /// Touch slot, or `-1` when the backend has no slot.
        slot: i32,
        /// Lifecycle marker.
        phase: TouchPhase,
        /// Backend-space x coordinate when supplied by the event.
        x: Option<f64>,
        /// Backend-space y coordinate when supplied by the event.
        y: Option<f64>,
        /// Device identifier.
        device: DeviceId,
    },
    /// Focus changed at the compositor seat boundary.
    FocusChanged {
        /// Current output, if any.
        output: Option<String>,
        /// Current client/surface token, if any.
        surface: Option<String>,
        /// Whether the seat is active and may receive focus.
        active: bool,
    },
    /// A previously active pointer/keyboard grab must be released.
    GrabCancelled {
        /// Token assigned by the compositor integration.
        token: u64,
        /// Reason for cancellation.
        reason: GrabCancelReason,
    },
}

/// Configuration for compositor-owned key repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRepeatConfig {
    /// Delay before the first repeat, in milliseconds.
    pub delay_ms: u64,
    /// Interval between repeat events, in milliseconds.
    pub interval_ms: u64,
    /// Maximum repeats emitted by one timer tick.
    pub max_burst: u8,
}

impl Default for KeyRepeatConfig {
    fn default() -> Self {
        Self {
            delay_ms: DEFAULT_REPEAT_DELAY_MS,
            interval_ms: DEFAULT_REPEAT_INTERVAL_MS,
            max_burst: DEFAULT_REPEAT_BURST,
        }
    }
}

impl KeyRepeatConfig {
    fn sanitized(self) -> Self {
        Self {
            delay_ms: self.delay_ms.min(60_000),
            interval_ms: self.interval_ms.max(1).min(1_000),
            max_burst: self.max_burst.max(1).min(32),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldKey {
    keycode: u32,
    device: DeviceId,
    next_repeat_ms: u64,
}

/// Pure, bounded key-repeat state machine.
#[derive(Debug, Clone)]
pub struct KeyRepeater {
    config: KeyRepeatConfig,
    held: Option<HeldKey>,
    last_time_ms: u64,
}

impl Default for KeyRepeater {
    fn default() -> Self {
        Self::new(KeyRepeatConfig::default())
    }
}

impl KeyRepeater {
    /// Create a repeat state machine with a sanitized configuration.
    pub fn new(config: KeyRepeatConfig) -> Self {
        Self {
            config: config.sanitized(),
            held: None,
            last_time_ms: 0,
        }
    }

    /// Return the effective configuration.
    pub fn config(&self) -> KeyRepeatConfig {
        self.config
    }

    /// Consume a physical key transition.
    pub fn key(&mut self, sample: KeySample) -> Vec<KeyEvent> {
        let time_us = self.monotonic_time_us(sample.time_us);
        let time_ms = time_us / 1_000;
        match sample.state {
            PhysicalKeyState::Pressed => {
                // A new press supersedes a previous held key.  This is the
                // safe behavior for rollover and for a device disappearing
                // without sending a release.
                self.held = Some(HeldKey {
                    keycode: sample.keycode,
                    device: sample.device.clone(),
                    next_repeat_ms: time_ms.saturating_add(self.config.delay_ms),
                });
                vec![KeyEvent {
                    time_us,
                    keycode: sample.keycode,
                    state: KeyDelivery::Pressed,
                    device: sample.device,
                }]
            }
            PhysicalKeyState::Released => {
                let matches_held = self
                    .held
                    .as_ref()
                    .is_some_and(|held| held.keycode == sample.keycode && held.device == sample.device);
                if matches_held {
                    self.held = None;
                }
                vec![KeyEvent {
                    time_us,
                    keycode: sample.keycode,
                    state: KeyDelivery::Released,
                    device: sample.device,
                }]
            }
        }
    }

    /// Generate bounded repeat events up to `now_ms`.
    pub fn tick(&mut self, now_ms: u64) -> Vec<KeyEvent> {
        let now_ms = self.last_time_ms.max(now_ms);
        self.last_time_ms = now_ms;
        let Some(held) = self.held.as_mut() else {
            return Vec::new();
        };
        if now_ms < held.next_repeat_ms {
            return Vec::new();
        }

        let mut events = Vec::with_capacity(self.config.max_burst as usize);
        for _ in 0..self.config.max_burst {
            if held.next_repeat_ms > now_ms {
                break;
            }
            let repeat_ms = held.next_repeat_ms;
            events.push(KeyEvent {
                time_us: repeat_ms.saturating_mul(1_000),
                keycode: held.keycode,
                state: KeyDelivery::Repeat,
                device: held.device.clone(),
            });
            held.next_repeat_ms = held.next_repeat_ms.saturating_add(self.config.interval_ms);
        }

        // Do not let a long pause create an unbounded backlog on the next
        // frame.  The next event remains in the future relative to now.
        if held.next_repeat_ms <= now_ms {
            held.next_repeat_ms = now_ms.saturating_add(self.config.interval_ms);
        }
        events
    }

    /// Stop repeating without fabricating a release event.
    pub fn cancel(&mut self) {
        self.held = None;
    }

    fn monotonic_time_us(&mut self, time_us: u64) -> u64 {
        let previous = self.last_time_ms.saturating_mul(1_000);
        let monotonic = previous.max(time_us);
        self.last_time_ms = monotonic / 1_000;
        monotonic
    }
}

/// Seat focus state owned by the compositor, not by a client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusState {
    /// Whether the native seat/host is active.
    pub active: bool,
    /// Current output name.
    pub output: Option<String>,
    /// Current surface token.
    pub surface: Option<String>,
}

/// A normalized seat input state machine.
#[derive(Debug, Clone)]
pub struct InputNormalizer {
    repeat: KeyRepeater,
    focus: FocusState,
    active_grab: Option<u64>,
    devices: HashMap<DeviceId, DeviceKind>,
    last_time_us: u64,
}

impl Default for InputNormalizer {
    fn default() -> Self {
        Self::new(KeyRepeatConfig::default())
    }
}

impl InputNormalizer {
    /// Create a normalizer with the given key-repeat policy.
    pub fn new(repeat: KeyRepeatConfig) -> Self {
        Self {
            repeat: KeyRepeater::new(repeat),
            focus: FocusState::default(),
            active_grab: None,
            devices: HashMap::new(),
            last_time_us: 0,
        }
    }

    /// Access current focus without allowing callers to mutate it silently.
    pub fn focus(&self) -> &FocusState {
        &self.focus
    }

    /// Access the currently active grab token, if any.
    pub fn active_grab(&self) -> Option<u64> {
        self.active_grab
    }

    /// Look up the last known device kind.
    pub fn device_kind(&self, device: &str) -> DeviceKind {
        self.devices.get(device).copied().unwrap_or(DeviceKind::Unknown)
    }

    /// Register a device and emit its normalized arrival event.
    pub fn device_added(
        &mut self,
        device: impl Into<DeviceId>,
        name: impl Into<String>,
        kind: DeviceKind,
    ) -> NormalizedInputEvent {
        let device = device.into();
        self.devices.insert(device.clone(), kind);
        NormalizedInputEvent::DeviceAdded {
            device,
            name: name.into(),
            kind,
        }
    }

    /// Remove a device and cancel any grab which could retain stale state.
    pub fn device_removed(&mut self, device: impl Into<DeviceId>) -> Vec<NormalizedInputEvent> {
        let device = device.into();
        let kind = self.devices.remove(&device).unwrap_or(DeviceKind::Unknown);
        self.repeat.cancel();
        let mut events = vec![NormalizedInputEvent::DeviceRemoved { device, kind }];
        if let Some(cancelled) = self.cancel_grab(GrabCancelReason::DeviceRemoved) {
            events.push(cancelled);
        }
        events
    }

    /// Normalize one physical key event and arm/cancel compositor repeat.
    pub fn keyboard(&mut self, sample: KeySample) -> Vec<NormalizedInputEvent> {
        let key_events = self.repeat.key(sample);
        key_events
            .into_iter()
            .map(NormalizedInputEvent::Keyboard)
            .collect()
    }

    /// Emit bounded repeat events for the current timer time.
    pub fn repeat_tick(&mut self, now_ms: u64) -> Vec<NormalizedInputEvent> {
        self.repeat
            .tick(now_ms)
            .into_iter()
            .map(NormalizedInputEvent::Keyboard)
            .collect()
    }

    /// Change focus atomically and cancel a grab when focus disappears.
    pub fn set_focus(
        &mut self,
        output: Option<impl Into<String>>,
        surface: Option<impl Into<String>>,
    ) -> Vec<NormalizedInputEvent> {
        let next = FocusState {
            active: self.focus.active,
            output: output.map(Into::into),
            surface: surface.map(Into::into),
        };
        if self.focus == next {
            return Vec::new();
        }

        let mut events = Vec::new();
        if next.output.is_none() || next.surface.is_none() {
            if let Some(cancelled) = self.cancel_grab(GrabCancelReason::FocusLost) {
                events.push(cancelled);
            }
        }
        self.focus.output = next.output.clone();
        self.focus.surface = next.surface.clone();
        events.push(NormalizedInputEvent::FocusChanged {
            output: next.output,
            surface: next.surface,
            active: self.focus.active,
        });
        events
    }

    /// Mark the seat active or inactive, cancelling all transient state when
    /// it is paused by a VT switch, suspend, or host deactivation.
    pub fn set_seat_active(&mut self, active: bool, reason: GrabCancelReason) -> Vec<NormalizedInputEvent> {
        if self.focus.active == active {
            return Vec::new();
        }
        self.focus.active = active;
        self.repeat.cancel();
        let mut events = Vec::new();
        if !active {
            if let Some(cancelled) = self.cancel_grab(reason) {
                events.push(cancelled);
            }
            self.focus.output = None;
            self.focus.surface = None;
        }
        events.push(NormalizedInputEvent::FocusChanged {
            output: self.focus.output.clone(),
            surface: self.focus.surface.clone(),
            active,
        });
        events
    }

    /// Start or replace an input grab.  Zero is reserved for “no grab”.
    pub fn begin_grab(&mut self, token: u64) -> Vec<NormalizedInputEvent> {
        if token == 0 {
            return Vec::new();
        }
        let mut events = Vec::new();
        if self.active_grab.is_some() {
            if let Some(cancelled) = self.cancel_grab(GrabCancelReason::Replaced) {
                events.push(cancelled);
            }
        }
        self.active_grab = Some(token);
        events
    }

    /// Cancel the active grab, if there is one.
    pub fn cancel_grab(&mut self, reason: GrabCancelReason) -> Option<NormalizedInputEvent> {
        self.active_grab
            .take()
            .map(|token| NormalizedInputEvent::GrabCancelled { token, reason })
    }

    /// Normalize a relative motion sample from a non-native backend.
    pub fn pointer_motion(
        &mut self,
        time_us: u64,
        dx: f64,
        dy: f64,
        unaccelerated_dx: f64,
        unaccelerated_dy: f64,
        device: impl Into<DeviceId>,
    ) -> NormalizedInputEvent {
        NormalizedInputEvent::PointerMotion {
            time_us: self.timestamp(time_us),
            dx: sanitize_delta(dx),
            dy: sanitize_delta(dy),
            unaccelerated_dx: sanitize_delta(unaccelerated_dx),
            unaccelerated_dy: sanitize_delta(unaccelerated_dy),
            device: device.into(),
        }
    }

    fn timestamp(&mut self, time_us: u64) -> u64 {
        self.last_time_us = self.last_time_us.max(time_us);
        self.last_time_us
    }

    #[cfg(feature = "native-session")]
    fn normalize_smithay_event(
        &mut self,
        event: smithay::backend::input::InputEvent<smithay::backend::libinput::LibinputInputBackend>,
    ) -> Vec<NormalizedInputEvent> {
        use smithay::backend::input::{
            AbsolutePositionEvent, Axis as SmithayAxis, AxisSource as SmithayAxisSource,
            Device as SmithayDevice, Event as SmithayEvent, GestureBeginEvent, GestureEndEvent,
            GesturePinchUpdateEvent, GestureSwipeUpdateEvent, InputEvent as SmithayInputEvent,
            KeyState as SmithayKeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
            PointerMotionEvent, TouchEvent,
        };

        let finite = |value: f64| sanitize_delta(value);
        match event {
            SmithayInputEvent::DeviceAdded { device } => {
                let kind = classify_smithay_device(&device);
                vec![self.device_added(device.id(), device.name(), kind)]
            }
            SmithayInputEvent::DeviceRemoved { device } => self.device_removed(device.id()),
            SmithayInputEvent::Keyboard { event } => {
                let device = event.device();
                self.keyboard(KeySample {
                    time_us: event.time(),
                    keycode: event.key_code().raw(),
                    state: match event.state() {
                        SmithayKeyState::Pressed => PhysicalKeyState::Pressed,
                        SmithayKeyState::Released => PhysicalKeyState::Released,
                    },
                    device: device.id(),
                })
            }
            SmithayInputEvent::PointerMotion { event } => {
                let device = event.device();
                vec![self.pointer_motion(
                    event.time(),
                    finite(event.delta_x()),
                    finite(event.delta_y()),
                    finite(event.delta_x_unaccel()),
                    finite(event.delta_y_unaccel()),
                    device.id(),
                )]
            }
            SmithayInputEvent::PointerMotionAbsolute { event } => {
                let device = event.device();
                vec![NormalizedInputEvent::PointerMotionAbsolute {
                    time_us: self.timestamp(event.time()),
                    x: finite(event.x()),
                    y: finite(event.y()),
                    device: device.id(),
                }]
            }
            SmithayInputEvent::PointerButton { event } => {
                let device = event.device();
                vec![NormalizedInputEvent::PointerButton {
                    time_us: self.timestamp(event.time()),
                    button: event.button_code(),
                    pressed: matches!(event.state(), smithay::backend::input::ButtonState::Pressed),
                    device: device.id(),
                }]
            }
            SmithayInputEvent::PointerAxis { event } => {
                let device = event.device();
                let source = match event.source() {
                    SmithayAxisSource::Wheel => AxisSource::Wheel,
                    SmithayAxisSource::Finger => AxisSource::Finger,
                    SmithayAxisSource::Continuous => AxisSource::Continuous,
                    SmithayAxisSource::WheelTilt => AxisSource::Wheel,
                };
                vec![NormalizedInputEvent::PointerAxis {
                    time_us: self.timestamp(event.time()),
                    horizontal: finite(event.amount(SmithayAxis::Horizontal).unwrap_or(0.0)),
                    vertical: finite(event.amount(SmithayAxis::Vertical).unwrap_or(0.0)),
                    horizontal_v120: event.amount_v120(SmithayAxis::Horizontal).map(finite),
                    vertical_v120: event.amount_v120(SmithayAxis::Vertical).map(finite),
                    source,
                    device: device.id(),
                }]
            }
            SmithayInputEvent::GestureSwipeBegin { event } => normalize_swipe(
                self,
                event.time(),
                GesturePhase::Begin,
                event.fingers(),
                0.0,
                0.0,
                event.device().id(),
            ),
            SmithayInputEvent::GestureSwipeUpdate { event } => normalize_swipe(
                self,
                event.time(),
                GesturePhase::Update,
                event.fingers(),
                finite(event.delta_x()),
                finite(event.delta_y()),
                event.device().id(),
            ),
            SmithayInputEvent::GestureSwipeEnd { event } => normalize_swipe(
                self,
                event.time(),
                GesturePhase::End {
                    cancelled: event.cancelled(),
                },
                event.fingers(),
                0.0,
                0.0,
                event.device().id(),
            ),
            SmithayInputEvent::GesturePinchBegin { event } => normalize_pinch(
                self,
                event.time(),
                GesturePhase::Begin,
                event.fingers(),
                0.0,
                0.0,
                1.0,
                0.0,
                event.device().id(),
            ),
            SmithayInputEvent::GesturePinchUpdate { event } => normalize_pinch(
                self,
                event.time(),
                GesturePhase::Update,
                event.fingers(),
                finite(event.delta_x()),
                finite(event.delta_y()),
                finite(event.scale()),
                finite(event.rotation()),
                event.device().id(),
            ),
            SmithayInputEvent::GesturePinchEnd { event } => normalize_pinch(
                self,
                event.time(),
                GesturePhase::End {
                    cancelled: event.cancelled(),
                },
                event.fingers(),
                0.0,
                0.0,
                1.0,
                0.0,
                event.device().id(),
            ),
            SmithayInputEvent::GestureHoldBegin { event } => vec![NormalizedInputEvent::GestureHold {
                time_us: self.timestamp(event.time()),
                phase: GesturePhase::Begin,
                fingers: event.fingers(),
                device: event.device().id(),
            }],
            SmithayInputEvent::GestureHoldEnd { event } => vec![NormalizedInputEvent::GestureHold {
                time_us: self.timestamp(event.time()),
                phase: GesturePhase::End {
                    cancelled: event.cancelled(),
                },
                fingers: event.fingers(),
                device: event.device().id(),
            }],
            SmithayInputEvent::TouchDown { event } => normalize_touch(
                self,
                event.time(),
                event.slot().into(),
                TouchPhase::Down,
                Some(finite(event.x())),
                Some(finite(event.y())),
                event.device().id(),
            ),
            SmithayInputEvent::TouchMotion { event } => normalize_touch(
                self,
                event.time(),
                event.slot().into(),
                TouchPhase::Motion,
                Some(finite(event.x())),
                Some(finite(event.y())),
                event.device().id(),
            ),
            SmithayInputEvent::TouchUp { event } => normalize_touch(
                self,
                event.time(),
                event.slot().into(),
                TouchPhase::Up,
                None,
                None,
                event.device().id(),
            ),
            SmithayInputEvent::TouchCancel { event } => normalize_touch(
                self,
                event.time(),
                event.slot().into(),
                TouchPhase::Cancel,
                None,
                None,
                event.device().id(),
            ),
            SmithayInputEvent::TouchFrame { event } => vec![NormalizedInputEvent::Touch {
                time_us: self.timestamp(event.time()),
                slot: -1,
                phase: TouchPhase::Frame,
                x: None,
                y: None,
                device: event.device().id(),
            }],
            // Switches/tablets are deliberately not turned into fake pointer
            // events.  The integrator may add them without changing the
            // ordering/focus guarantees of this stream.
            SmithayInputEvent::TabletToolAxis { .. }
            | SmithayInputEvent::TabletToolProximity { .. }
            | SmithayInputEvent::TabletToolTip { .. }
            | SmithayInputEvent::TabletToolButton { .. }
            | SmithayInputEvent::SwitchToggle { .. }
            | SmithayInputEvent::Special(_) => Vec::new(),
        }
    }
}

fn sanitize_delta(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(-MAX_RELATIVE_DELTA, MAX_RELATIVE_DELTA)
    } else {
        0.0
    }
}

#[cfg(feature = "native-session")]
fn classify_smithay_device(
    device: &<smithay::backend::libinput::LibinputInputBackend as smithay::backend::input::InputBackend>::Device,
) -> DeviceKind {
    use smithay::backend::input::{Device as SmithayDevice, DeviceCapability};
    classify_device_capabilities(
        device.has_capability(DeviceCapability::Keyboard),
        device.has_capability(DeviceCapability::Pointer),
        device.has_capability(DeviceCapability::Touch),
        device.has_capability(DeviceCapability::TabletTool),
        device.has_capability(DeviceCapability::Gesture),
    )
}

#[cfg(feature = "native-session")]
fn normalize_swipe(
    normalizer: &mut InputNormalizer,
    time_us: u64,
    phase: GesturePhase,
    fingers: u32,
    dx: f64,
    dy: f64,
    device: DeviceId,
) -> Vec<NormalizedInputEvent> {
    vec![NormalizedInputEvent::GestureSwipe {
        time_us: normalizer.timestamp(time_us),
        phase,
        fingers,
        dx: sanitize_delta(dx),
        dy: sanitize_delta(dy),
        device,
    }]
}

#[cfg(feature = "native-session")]
fn normalize_pinch(
    normalizer: &mut InputNormalizer,
    time_us: u64,
    phase: GesturePhase,
    fingers: u32,
    dx: f64,
    dy: f64,
    scale: f64,
    rotation: f64,
    device: DeviceId,
) -> Vec<NormalizedInputEvent> {
    vec![NormalizedInputEvent::GesturePinch {
        time_us: normalizer.timestamp(time_us),
        phase,
        fingers,
        dx: sanitize_delta(dx),
        dy: sanitize_delta(dy),
        scale: if scale.is_finite() {
            scale.max(0.01).min(100.0)
        } else {
            1.0
        },
        rotation: sanitize_delta(rotation),
        device,
    }]
}

#[cfg(feature = "native-session")]
fn normalize_touch(
    normalizer: &mut InputNormalizer,
    time_us: u64,
    slot: i32,
    phase: TouchPhase,
    x: Option<f64>,
    y: Option<f64>,
    device: DeviceId,
) -> Vec<NormalizedInputEvent> {
    vec![NormalizedInputEvent::Touch {
        time_us: normalizer.timestamp(time_us),
        slot,
        phase,
        x: x.map(sanitize_delta),
        y: y.map(sanitize_delta),
        device,
    }]
}

/// An error raised before a native libinput source can be installed.
#[cfg(feature = "native-session")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeInputError(String);

#[cfg(feature = "native-session")]
impl NativeInputError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(feature = "native-session")]
impl fmt::Display for NativeInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(feature = "native-session")]
impl std::error::Error for NativeInputError {}

/// A calloop source which owns the Smithay 0.7 libinput backend and emits
/// normalized Rouch events directly.
#[cfg(feature = "native-session")]
#[derive(Debug)]
pub struct LibinputSeatSource {
    backend: smithay::backend::libinput::LibinputInputBackend,
    normalizer: InputNormalizer,
    seat_name: String,
}

#[cfg(feature = "native-session")]
impl LibinputSeatSource {
    /// Initialize libinput through a Smithay `Session` implementation.
    ///
    /// The session is moved into Smithay's `LibinputSessionInterface`, so
    /// devices are opened through libseat/logind rather than direct `/dev`
    /// access.  The separate libseat notifier still needs to be installed by
    /// the integrator and forwarded to [`Self::session_event`].
    pub fn new<S>(session: S, seat: impl Into<String>) -> Result<Self, NativeInputError>
    where
        S: smithay::backend::session::Session + 'static,
    {
        let seat_name = seat.into();
        let interface = smithay::backend::libinput::LibinputSessionInterface::from(session);
        let mut context = smithay::reexports::input::Libinput::new_with_udev(interface);
        context
            .udev_assign_seat(&seat_name)
            .map_err(|error| NativeInputError::new(format!("libinput seat assignment failed: {error:?}")))?;

        Ok(Self {
            backend: smithay::backend::libinput::LibinputInputBackend::new(context),
            normalizer: InputNormalizer::default(),
            seat_name,
        })
    }

    /// Name of the seat assigned to this source.
    pub fn seat_name(&self) -> &str {
        &self.seat_name
    }

    /// Access the normalizer for timer ticks, focus and grab integration.
    pub fn normalizer(&self) -> &InputNormalizer {
        &self.normalizer
    }

    /// Mutably access the normalizer for compositor integration.
    pub fn normalizer_mut(&mut self) -> &mut InputNormalizer {
        &mut self.normalizer
    }

    /// Forward a session pause/resume from Smithay's libseat notifier.
    pub fn session_event(&mut self, event: smithay::backend::session::Event) -> Vec<NormalizedInputEvent> {
        match event {
            smithay::backend::session::Event::PauseSession => self
                .normalizer
                .set_seat_active(false, GrabCancelReason::SeatPaused),
            smithay::backend::session::Event::ActivateSession => self
                .normalizer
                .set_seat_active(true, GrabCancelReason::SeatPaused),
        }
    }

    /// Forward host deactivation when native output/session focus is lost.
    pub fn host_deactivated(&mut self) -> Vec<NormalizedInputEvent> {
        self.normalizer
            .set_seat_active(false, GrabCancelReason::HostDeactivated)
    }
}

#[cfg(feature = "native-session")]
impl smithay::reexports::calloop::EventSource for LibinputSeatSource {
    type Event = NormalizedInputEvent;
    type Metadata = ();
    type Ret = ();
    type Error = std::io::Error;

    fn process_events<F>(
        &mut self,
        readiness: smithay::reexports::calloop::Readiness,
        token: smithay::reexports::calloop::Token,
        mut callback: F,
    ) -> Result<smithay::reexports::calloop::PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        use smithay::reexports::calloop::EventSource;
        self.backend.process_events(readiness, token, |event, _| {
            let normalized = self.normalizer.normalize_smithay_event(event);
            for event in normalized {
                callback(event, &mut ());
            }
        })
    }

    fn register(
        &mut self,
        poll: &mut smithay::reexports::calloop::Poll,
        factory: &mut smithay::reexports::calloop::TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        use smithay::reexports::calloop::EventSource;
        self.backend.register(poll, factory)
    }

    fn reregister(
        &mut self,
        poll: &mut smithay::reexports::calloop::Poll,
        factory: &mut smithay::reexports::calloop::TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        use smithay::reexports::calloop::EventSource;
        self.backend.reregister(poll, factory)
    }

    fn unregister(
        &mut self,
        poll: &mut smithay::reexports::calloop::Poll,
    ) -> smithay::reexports::calloop::Result<()> {
        use smithay::reexports::calloop::EventSource;
        self.backend.unregister(poll)
    }
}

#[cfg(feature = "native-session")]
impl InputNormalizer {
    /// Public bridge used by a caller that keeps Smithay's event source
    /// separate from the normalizer.
    pub fn normalize_native_event(
        &mut self,
        event: smithay::backend::input::InputEvent<smithay::backend::libinput::LibinputInputBackend>,
    ) -> Vec<NormalizedInputEvent> {
        self.normalize_smithay_event(event)
    }
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Keyboard => "keyboard",
            Self::Mouse => "mouse",
            Self::Touchpad => "touchpad",
            Self::Touchscreen => "touchscreen",
            Self::Tablet => "tablet",
            Self::Unknown => "unknown",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(time_us: u64, keycode: u32, state: PhysicalKeyState) -> KeySample {
        KeySample {
            time_us,
            keycode,
            state,
            device: "kbd-0".into(),
        }
    }

    #[test]
    fn classifies_touchpad_before_mouse() {
        assert_eq!(
            classify_device_capabilities(false, true, false, false, true),
            DeviceKind::Touchpad
        );
        assert_eq!(
            classify_device_capabilities(false, true, false, false, false),
            DeviceKind::Mouse
        );
        assert_eq!(
            classify_device_capabilities(false, true, true, false, true),
            DeviceKind::Touchscreen
        );
    }

    #[test]
    fn backend_selection_has_native_and_nested_fallback() {
        assert_eq!(select_input_backend(true, true), InputBackendChoice::Libinput);
        assert_eq!(select_input_backend(false, true), InputBackendChoice::NestedHost);
        assert_eq!(
            select_input_backend(false, false),
            InputBackendChoice::Unavailable
        );
    }

    #[test]
    fn key_repeat_starts_after_delay_and_is_bounded() {
        let mut repeater = KeyRepeater::new(KeyRepeatConfig {
            delay_ms: 100,
            interval_ms: 10,
            max_burst: 2,
        });
        assert_eq!(
            repeater.key(key(0, 30, PhysicalKeyState::Pressed))[0].state,
            KeyDelivery::Pressed
        );
        assert!(repeater.tick(1).is_empty());
        assert_eq!(repeater.tick(100).len(), 1);
        // A large timer jump cannot generate an unbounded vector.
        assert_eq!(repeater.tick(1_000).len(), 2);
        assert_eq!(
            repeater.key(key(1_100_000, 30, PhysicalKeyState::Released))[0].state,
            KeyDelivery::Released
        );
        assert!(repeater.tick(2_000).is_empty());
    }

    #[test]
    fn time_is_monotonic_and_bad_motion_falls_back_to_zero() {
        let mut normalizer = InputNormalizer::default();
        let first = normalizer.pointer_motion(20, f64::NAN, f64::INFINITY, -9_000.0, 3.0, "mouse");
        assert!(matches!(
            first,
            NormalizedInputEvent::PointerMotion {
                time_us: 20,
                dx: 0.0,
                dy: 0.0,
                unaccelerated_dx: -8_192.0,
                ..
            }
        ));
        let second = normalizer.pointer_motion(10, 1.0, 1.0, 1.0, 1.0, "mouse");
        assert!(matches!(
            second,
            NormalizedInputEvent::PointerMotion { time_us: 20, .. }
        ));
    }

    #[test]
    fn visual_motion_gate_ignores_duplicate_subpixel_samples() {
        assert!(!pointer_visual_motion_changed((24.01, 8.41), (24.20, 8.45)));
        assert!(pointer_visual_motion_changed((24.49, 8.49), (25.01, 8.49)));
        assert!(pointer_visual_motion_changed((24.0, 8.0), (f64::NAN, 8.0)));
        assert_eq!(VisualPointerPosition::from_f64(f64::INFINITY, 1.0), None);
    }

    #[test]
    fn only_primary_pointer_button_can_activate_shell_chrome() {
        assert!(is_primary_pointer_button(PRIMARY_POINTER_BUTTON));
        assert!(!is_primary_pointer_button(PRIMARY_POINTER_BUTTON + 1));
    }

    #[test]
    fn shell_shortcuts_are_explicit_and_do_not_claim_plain_text() {
        assert_eq!(
            shell_shortcut(0xff09, true, false, false, false),
            Some(ShellShortcut::CycleWindows { reverse: false })
        );
        assert_eq!(
            shell_shortcut(0xff09, false, false, true, true),
            Some(ShellShortcut::CycleWindows { reverse: true })
        );
        assert_eq!(
            shell_shortcut(0xfe20, true, false, false, true),
            Some(ShellShortcut::CycleWindows { reverse: true })
        );
        assert_eq!(
            shell_shortcut(0xff51, true, true, false, false),
            Some(ShellShortcut::SwitchWorkspace { delta: -1 })
        );
        assert_eq!(
            shell_shortcut(b'3' as u32, true, false, false, false),
            Some(ShellShortcut::FocusWindow { number: 3 })
        );
        assert_eq!(
            shell_shortcut(b'G' as u32, true, false, false, true),
            Some(ShellShortcut::ToggleGallery)
        );
        assert_eq!(shell_shortcut(b'x' as u32, false, false, false, false), None);
        assert_eq!(
            shell_shortcut(0xff1b, false, false, false, false),
            Some(ShellShortcut::DismissOverlay)
        );
    }

    #[test]
    fn focus_loss_cancels_grab_before_focus_event() {
        let mut normalizer = InputNormalizer::default();
        normalizer.set_seat_active(true, GrabCancelReason::SeatPaused);
        normalizer.set_focus(Some("eDP-1"), Some("surface-1"));
        assert!(normalizer.begin_grab(7).is_empty());
        let events = normalizer.set_focus(Some("eDP-1"), None::<String>);
        assert!(matches!(
            events.as_slice(),
            [
                NormalizedInputEvent::GrabCancelled {
                    token: 7,
                    reason: GrabCancelReason::FocusLost
                },
                NormalizedInputEvent::FocusChanged { surface: None, .. }
            ]
        ));
        assert_eq!(normalizer.active_grab(), None);
    }

    #[test]
    fn device_removal_cancels_repeat_and_grab() {
        let mut normalizer = InputNormalizer::default();
        let _ = normalizer.device_added("mouse", "Mouse", DeviceKind::Mouse);
        normalizer.keyboard(key(0, 40, PhysicalKeyState::Pressed));
        normalizer.begin_grab(99);
        let events = normalizer.device_removed("mouse");
        assert!(matches!(events[0], NormalizedInputEvent::DeviceRemoved { .. }));
        assert!(matches!(
            events[1],
            NormalizedInputEvent::GrabCancelled {
                token: 99,
                reason: GrabCancelReason::DeviceRemoved
            }
        ));
        assert!(normalizer.repeat_tick(60_000).is_empty());
    }
}
