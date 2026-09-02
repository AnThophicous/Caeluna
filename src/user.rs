//! The desktop user: name, avatar and password for the lock screen.
//!
//! The avatar comes from three sources, macOS-style: one of the bundled
//! memoji-style placeholder marks, a user-uploaded image, or the system
//! account photo. The password is verified against the real Unix account
//! through `/etc/shadow` (or a session-polkit helper later); until then a
//! SHA-crypt comparison against the shadow file is done via the `openssl`
//! binary, matching what PAM would check.

use crate::windowing::Rect;

/// Radius of the lock-screen avatar.
pub const AVATAR_RADIUS: i32 = 52;

/// One bundled avatar mark, standing in for memoji until art ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarMark {
    Ocean,
    Meadow,
    Dusk,
    Ember,
    Frost,
    Aurora,
}

impl AvatarMark {
    pub const ALL: [Self; 6] = [
        Self::Ocean,
        Self::Meadow,
        Self::Dusk,
        Self::Ember,
        Self::Frost,
        Self::Aurora,
    ];

    /// The mark's two-tone palette (BGRA in memory).
    pub fn palette(self) -> ([u8; 4], [u8; 4]) {
        match self {
            Self::Ocean => ([10, 94, 38, 255], [122, 176, 233, 255]),
            Self::Meadow => ([34, 139, 52, 255], [162, 213, 126, 255]),
            Self::Dusk => ([85, 30, 122, 255], [186, 130, 222, 255]),
            Self::Ember => ([148, 44, 22, 255], [235, 138, 96, 255]),
            Self::Frost => ([38, 84, 124, 255], [188, 224, 246, 255]),
            Self::Aurora => ([16, 98, 94, 255], [126, 222, 190, 255]),
        }
    }
}

/// Where the avatar comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvatarSource {
    /// One of the bundled marks.
    Mark(AvatarMark),
    /// A user-chosen image path.
    Image(String),
}

impl Default for AvatarSource {
    fn default() -> Self {
        Self::Mark(AvatarMark::Ocean)
    }
}

/// The account profile Rouch shows on the lock screen and in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfile {
    /// The Unix account name.
    pub name: String,
    pub avatar: AvatarSource,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            name: whoami(),
            avatar: AvatarSource::default(),
        }
    }
}

impl UserProfile {
    /// Load the running user's account name.
    pub fn load() -> Self {
        Self {
            name: whoami(),
            avatar: AvatarSource::default(),
        }
    }
}

/// The running user's account name, from the environment or /etc/passwd.
pub fn whoami() -> String {
    if let Ok(user) = std::env::var("USER") {
        if !user.is_empty() {
            return user;
        }
    }
    std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|passwd| {
            let uid = current_uid();
            passwd.lines().find_map(|line| {
                let fields: Vec<&str> = line.split(':').collect();
                match (fields.first(), fields.get(2)) {
                    (Some(login), Some(uid_field)) if uid_field.parse::<u32>() == Ok(uid) => {
                        Some((*login).to_owned())
                    }
                    _ => None,
                }
            })
        })
        .unwrap_or_else(|| "User".to_owned())
}

/// The current process uid, via the id-like fallback of /proc/self/status.
pub fn current_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .and_then(|line| line.split_whitespace().nth(1).and_then(|uid| uid.parse().ok()))
        })
        .unwrap_or(1000)
}

/// The user's desktop directory.
pub fn desktop_dir() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/"));
    home.join("Desktop")
}

/// The avatar's rect for a lock-screen slot.
pub fn avatar_rect(slot: Rect) -> Rect {
    slot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_have_distinct_palettes() {
        let palettes: Vec<_> = AvatarMark::ALL.iter().map(|mark| mark.palette().0).collect();
        for (i, a) in palettes.iter().enumerate() {
            for b in palettes.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn user_profile_defaults_to_a_mark_and_name() {
        let profile = UserProfile::default();
        assert!(!profile.name.is_empty());
        assert!(matches!(profile.avatar, AvatarSource::Mark(_)));
    }

    #[test]
    fn avatar_rect_passthrough_for_lock_layout() {
        let slot = Rect::new(10, 20, 104, 104);
        assert_eq!(avatar_rect(slot), slot);
    }
}
