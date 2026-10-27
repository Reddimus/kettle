// Included in build.rs and compiled in the library for tests.
// Keep imports out of this file: include! shares build.rs's namespace.

#[allow(dead_code)]
/// Exclude metadata and editor leftovers from the bundled theme list.
pub(crate) fn is_bundled_theme_filename(name: &str) -> bool {
    // Metadata shipped with the upstream theme collection.
    if matches!(name, "LICENSE" | "README.md") {
        return false;
    }
    if name.starts_with('.') {
        return false;
    }
    // Emacs autosaves need not start with a dot.
    if name.starts_with('#') {
        return false;
    }
    // Includes Office lock files such as ~$name.
    if name.starts_with('~') {
        return false;
    }
    // OS metadata matches case-insensitively; Finder's Icon\r includes a CR.
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "thumbs.db" | "desktop.ini" | "icon\r") {
        return false;
    }
    // Editor backup-file patterns by suffix.
    if lower.ends_with('~')
        || lower.ends_with(".bak")
        || lower.ends_with(".orig")
        || lower.ends_with(".swp")
        || lower.ends_with(".swo")
        || lower.ends_with(".tmp")
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::is_bundled_theme_filename;

    #[test]
    fn skips_metadata_dotfiles_and_backups() {
        // Real theme names — must be accepted.
        assert!(is_bundled_theme_filename("TokyoNight Night"));
        assert!(is_bundled_theme_filename("3024 Day"));
        assert!(is_bundled_theme_filename("0x96f"));
        assert!(is_bundled_theme_filename("12-bit Rainbow"));

        // Upstream metadata files that *do* live in assets/themes/.
        assert!(!is_bundled_theme_filename("LICENSE"));
        assert!(!is_bundled_theme_filename("README.md"));

        // Dotfiles from common OS / VCS / editor habits.
        assert!(!is_bundled_theme_filename(".DS_Store"));
        assert!(!is_bundled_theme_filename(".gitignore"));
        assert!(!is_bundled_theme_filename(".gitkeep"));
        assert!(!is_bundled_theme_filename(".directory"));
        assert!(!is_bundled_theme_filename(".#emacs-lock"));
        // Emacs autosave files are `#name#` — not a
        // dotfile, so the dotfile branch above misses them.
        assert!(!is_bundled_theme_filename("#TokyoNight Night#"));
        assert!(!is_bundled_theme_filename("#3024 Day#"));
        // Office lock files use `~$` prefix (`~$theme.docx`
        // shape). Any `~`-prefix is also not a real theme name; the
        // `~`-suffix vim backup is caught by the suffix branch.
        assert!(!is_bundled_theme_filename("~$TokyoNight Night"));
        assert!(!is_bundled_theme_filename("~TempTheme"));

        // Windows / Finder metadata that does NOT start with a dot.
        assert!(!is_bundled_theme_filename("Thumbs.db"));
        assert!(!is_bundled_theme_filename("desktop.ini"));
        // NTFS case-insensitive, so an upper- or mixed-case
        // form of the same file (a Windows checkout / Git Bash copy /
        // robocopy with mismatched casing) still has to be skipped.
        assert!(!is_bundled_theme_filename("THUMBS.DB"));
        assert!(!is_bundled_theme_filename("Thumbs.DB"));
        assert!(!is_bundled_theme_filename("Desktop.ini"));
        assert!(!is_bundled_theme_filename("DESKTOP.INI"));

        // Editor backup-file patterns.
        assert!(!is_bundled_theme_filename("MyTheme~"));
        assert!(!is_bundled_theme_filename("Solarized.bak"));
        assert!(!is_bundled_theme_filename("Solarized.orig"));
        assert!(!is_bundled_theme_filename("Solarized.swp"));
        assert!(
            !is_bundled_theme_filename("Solarized.SWP"),
            "case-insensitive"
        );
        assert!(!is_bundled_theme_filename("temp.tmp"));
    }
}
