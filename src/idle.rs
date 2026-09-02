//! The idle-to-rest-to-lock choreography, macOS Tahoe style.
//!
//! Ten minutes without input blanks the output and stops rendering — the
//! deepest power saving Rouch can reach from a compositor. Any input wakes
//! the session into the **rest screen**: a dimmed, calm view that is one
//! gesture away from the lock screen, which asks for the password. A rest
//! screen wake (Space or an upward drag) moves to the lock screen; typing
//! the password there unlocks.

use std::time::Duration;

/// Input idle threshold before the output blanks.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// The lock screen password length limit.
pub const PASSWORD_MAX: usize = 64;

/// The states of the session wake chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// Fully awake.
    Active,
    /// The output is off; any input wakes to the rest screen.
    Blank,
    /// The rest screen: Space or an upward drag moves to Lock.
    Rest,
    /// The lock screen: the password unlocks.
    Lock,
}

impl Session {
    /// Apply one input event to the session state.
    ///
    /// Presses wake a blank output into the rest screen; from the rest
    /// screen, Space advances to the lock screen, and any other press stays
    /// resting (macOS dims further). The lock screen consumes password
    /// input, so nothing here advances it.
    pub fn on_press(self) -> Self {
        match self {
            Self::Blank => Self::Rest,
            other => other,
        }
    }

    /// Apply one key event. Returns (next, key_was_password_input).
    pub fn on_key(self, key: RestAdvance) -> (Self, bool) {
        match self {
            Self::Blank => (Self::Rest, false),
            Self::Rest => match key {
                RestAdvance::Space | RestAdvance::Character => (Self::Lock, key == RestAdvance::Space),
                RestAdvance::Other => (Self::Rest, false),
            },
            Self::Lock => (Self::Lock, true),
            Self::Active => (Self::Active, false),
        }
    }

    /// Apply an upward drag on the rest screen.
    pub fn on_drag(self, dy: f32) -> Self {
        match self {
            Self::Rest if dy < -30.0 => Self::Lock,
            other => other,
        }
    }
}

/// How a key event maps onto the rest-screen advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestAdvance {
    Space,
    Character,
    Other,
}

/// The lock screen's password entry state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordEntry {
    pub buffer: String,
}

impl Default for PasswordEntry {
    fn default() -> Self {
        Self::new()
    }
}

impl PasswordEntry {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Append one character, enforcing the length limit.
    pub fn push(&mut self, character: char) {
        if self.buffer.chars().count() < PASSWORD_MAX {
            self.buffer.push(character);
        }
    }

    /// Remove the last character.
    pub fn backspace(&mut self) {
        self.buffer.pop();
    }

    /// Whether the entry can be verified yet.
    pub fn ready(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Consume and return the typed password for verification.
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.buffer)
    }
}

/// A decision from password verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockResult {
    Unlocked,
    Wrong,
}

/// The lock screen layout: the avatar, the name, the entry field.
pub fn lock_layout(
    work_area: crate::windowing::Rect,
) -> (
    crate::windowing::Rect,
    crate::windowing::Rect,
    crate::windowing::Rect,
) {
    let centre_x = work_area.origin.x + work_area.size.width / 2;
    let centre_y = work_area.origin.y + work_area.size.height / 2;

    let avatar = crate::windowing::Rect::new(centre_x - 52, centre_y - 140, 104, 104);
    let name = crate::windowing::Rect::new(centre_x - 160, avatar.bottom() + 18, 320, 26);
    let field = crate::windowing::Rect::new(centre_x - 180, name.bottom() + 14, 360, 36);
    (avatar, name, field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_timeout_is_ten_minutes() {
        assert_eq!(IDLE_TIMEOUT, Duration::from_secs(600));
    }

    #[test]
    fn any_press_wakes_the_blank_output_to_rest() {
        assert_eq!(Session::Blank.on_press(), Session::Rest);
        assert_eq!(Session::Active.on_press(), Session::Active);
        assert_eq!(Session::Lock.on_press(), Session::Lock);
    }

    #[test]
    fn space_or_typing_advances_rest_to_lock() {
        let (next, consumed) = Session::Rest.on_key(RestAdvance::Space);
        assert_eq!(next, Session::Lock);
        assert!(consumed);

        let (next, _) = Session::Rest.on_key(RestAdvance::Character);
        assert_eq!(next, Session::Lock);

        let (next, consumed) = Session::Rest.on_key(RestAdvance::Other);
        assert_eq!(next, Session::Rest);
        assert!(!consumed);
    }

    #[test]
    fn upward_drag_unlocks_rest_to_lock() {
        assert_eq!(Session::Rest.on_drag(-60.0), Session::Lock);
        assert_eq!(Session::Rest.on_drag(-10.0), Session::Rest);
        assert_eq!(Session::Rest.on_drag(60.0), Session::Rest);
        assert_eq!(Session::Active.on_drag(-60.0), Session::Active);
    }

    #[test]
    fn password_entry_edits_within_limit() {
        let mut entry = PasswordEntry::new();
        assert!(!entry.ready());
        for character in "hello".chars() {
            entry.push(character);
        }
        assert!(entry.ready());
        entry.backspace();
        assert_eq!(entry.buffer, "hell");
        assert_eq!(entry.take(), "hell");
        assert!(!entry.ready());
    }

    #[test]
    fn lock_layout_stacks_avatar_name_field() {
        let (avatar, name, field) = lock_layout(crate::windowing::Rect::new(0, 0, 1440, 900));
        assert!(avatar.bottom() < name.origin.y);
        assert!(name.bottom() < field.origin.y);
        let centre_x = 1440 / 2;
        assert_eq!(avatar.origin.x + avatar.size.width / 2, centre_x);
    }
}
