//! A parsed freedesktop `.desktop` entry: the application database unit.
//!
//! Rouch scans `/usr/share/applications` (and the user directory) once at
//! startup; the launcher, dock and settings all read from that database.

/// One parsed application entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// The file stem, e.g. `org.gnome.Calculator`.
    pub id: String,
    /// `Name=` â€” the display name.
    pub name: String,
    /// `Icon=` â€” a theme icon name or path.
    pub icon: String,
    /// `Exec=` â€” the command line, minus field codes.
    pub exec: String,
    /// `Categories=` â€” split on semicolons.
    pub categories: Vec<String>,
    /// `NoDisplay=true` hides the app from menus.
    pub no_display: bool,
}

impl DesktopEntry {
    /// Parse one `.desktop` file body. Only the desktop-entry group is read.
    pub fn parse(id: &str, body: &str) -> Self {
        let mut name = String::new();
        let mut icon = String::new();
        let mut exec = String::new();
        let mut categories: Vec<String> = Vec::new();
        let mut no_display = false;
        let mut in_entry = false;

        for line in body.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "Name" => name = value.to_owned(),
                "Icon" => icon = value.to_owned(),
                "Exec" => exec = value.to_owned(),
                "Categories" => {
                    categories = value
                        .split(';')
                        .map(str::trim)
                        .filter(|category| !category.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
                _ => {}
            }
        }

        Self {
            id: id.to_owned(),
            name,
            icon,
            exec,
            categories,
            no_display,
        }
    }

    /// Visible in menus?
    pub fn visible(&self) -> bool {
        !self.no_display && !self.name.is_empty() && !self.exec.is_empty()
    }

    /// The launch command as (program, args), with launch-time markers stripped.
    pub fn launch_parts(&self) -> (String, Vec<String>) {
        let mut parts = shlex_split(&self.exec).filter(|token| !is_launch_marker(token));
        let program = parts.next().unwrap_or_default();
        (program, parts.collect())
    }
}

/// Tokens that only mean something when files are being opened.
///
/// `%U`, `%f` and friends are the freedesktop field codes. `@@` and `@@u` are
/// Flatpak's file-forwarding markers: every entry Flatpak exports contains
/// `--file-forwarding app @@u %U @@`, and passing the markers through makes
/// `flatpak run` reject the command line. Rouch launches without a file
/// argument, so all of them are dropped together.
fn is_launch_marker(token: &str) -> bool {
    token.starts_with('%') || token == "@@" || token == "@@u"
}

/// A minimal shell-style splitter, honouring double quotes.
fn shlex_split(line: &str) -> impl Iterator<Item = String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for character in line.chars() {
        match character {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "[Desktop Entry]\nType=Application\nName=Text Editor\nExec=gnome-text-editor %U\nIcon=org.gnome.TextEditor\nCategories=Utility;TextEditor;\nNoDisplay=false\n\n[Other]\nName=Ignored\n";

    #[test]
    fn flatpak_file_forwarding_markers_never_reach_the_command_line() {
        let entry = DesktopEntry::parse(
            "org.videolan.VLC",
            "[Desktop Entry]\nName=VLC\nExec=/usr/bin/flatpak run --branch=stable --arch=x86_64 --file-forwarding org.videolan.VLC @@u %U @@\n",
        );
        let (program, args) = entry.launch_parts();
        assert_eq!(program, "/usr/bin/flatpak");
        assert_eq!(
            args,
            vec![
                "run".to_owned(),
                "--branch=stable".to_owned(),
                "--arch=x86_64".to_owned(),
                "--file-forwarding".to_owned(),
                "org.videolan.VLC".to_owned(),
            ]
        );
    }

    #[test]
    fn parses_the_desktop_entry_group_only() {
        let entry = DesktopEntry::parse("org.gnome.TextEditor", BODY);
        assert_eq!(entry.name, "Text Editor");
        assert_eq!(entry.icon, "org.gnome.TextEditor");
        assert_eq!(entry.categories, vec!["Utility", "TextEditor"]);
        assert!(!entry.no_display);
    }

    #[test]
    fn field_codes_are_stripped_from_exec() {
        let entry = DesktopEntry::parse("app", BODY);
        let (program, args) = entry.launch_parts();
        assert_eq!(program, "gnome-text-editor");
        assert!(args.is_empty());
    }

    #[test]
    fn quoted_args_survive_splitting() {
        let entry = DesktopEntry::parse(
            "x",
            "[Desktop Entry]\nName=X\nExec=run \"arg with spaces\" file\n",
        );
        let (_, args) = entry.launch_parts();
        assert_eq!(args, vec!["arg with spaces", "file"]);
    }

    #[test]
    fn hidden_entries_are_not_visible() {
        let hidden = DesktopEntry::parse("x", "Name=Hidden\nExec=run\nNoDisplay=true\n");
        assert!(!hidden.visible());
        assert!(DesktopEntry::parse("x", BODY).visible());
    }
}
