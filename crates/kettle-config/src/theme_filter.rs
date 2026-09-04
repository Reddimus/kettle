// Shared between `build.rs` (which calls this to filter `assets/themes/`
// entries at compile time) and the library (where it has unit tests).
// Don't add anything here that depends on `std::path::Path`, the build
// script imports this file via `include!` and the path types aren't
// always identical across build-script / library contexts.
//
// `#[allow(dead_code)]` — the function is only called from `build.rs`
// (which `include!`s this file); the lib copy is unused at runtime, the
// tests reach the same body through the `cfg(test)` mod below.

#[allow(dead_code)]
/// Return `true` when a filename in `assets/themes/` is a real theme file
/// the build script should bundle, `false` when it's metadata or junk that
/// happened to land in the directory.
///
/// The skip list catches:
/// - `LICENSE` / `README.md` (the upstream repo's own metadata).
/// - Any dotfile (`.DS_Store` on macOS, `.git*`, `.swp` editor swap files,
///   emacs `.#name` lock files).
/// - Emacs `#name#` autosave files (saved when a buffer crashes mid-edit;
///   no leading dot so they slip past the dotfile branch).
/// - Microsoft Office `~$name` lock files (sync-folder fallout when a
///   contributor edits a doc on OneDrive / SharePoint / Dropbox).
/// - Windows / macOS desktop metadata (`Thumbs.db`, `desktop.ini`,
///   `Icon\r`), case-insensitive.
/// - Backup-file patterns by suffix (`*~`, `*.bak`, `*.orig`, `*.swp`,
///   `*.swo`, `*.tmp`), case-insensitive.
///
/// Without this filter, a maintainer cloning the repo on macOS and
/// opening the themes folder in Finder would pollute the bundled theme
/// list with a "`.DS_Store`" entry; same shape for a Windows checkout
/// that picked up `Thumbs.db`. The bundled-themes count is publicly
/// surfaced (`kettle --list-themes`, README), so a phantom theme is a
/// real user-visible bug.
pub(crate) fn is_bundled_theme_filename(name: &str) -> bool {
    // Exact-name metadata files from the upstream iTerm2-Color-Schemes
    // ghostty/ directory; these are intentionally shipped alongside the
    // themes but aren't themes themselves.
    if matches!(name, "LICENSE" | "README.md") {
        return false;
    }
    // Dotfiles — every common one (`.DS_Store`, `.git*`, `.swp`,
    // `.swo`, `.directory`) starts with `.`. Theme names are
    // user-facing display names ("3024 Day", "TokyoNight Night") and
    // never start with a dot.
    if name.starts_with('.') {
        return false;
    }
    // Emacs autosave / lock-file prefix patterns. An
    // unsaved-buffer autosave is `#name#` (sandwiched between two
    // literal `#`), an in-progress lock is `.#name` (already caught
    // by the dotfile branch). Bundled theme names never legitimately
    // start with `#`. iTerm2 / Vim / nvim swap files also live as
    // `.name.swp`, again caught by the dotfile branch.
    if name.starts_with('#') {
        return false;
    }
    // Microsoft Office lock-file prefix. When you open a
    // `.docx`/`.xlsx`/`.pptx` from a network drive or shared folder,
    // Office writes a sibling hidden-style file `~$filename` to mark
    // the lock. Bundled themes never start with `~`. The `~`-suffix
    // (vim backup) is already caught by the suffix branch below;
    // this branch handles the *prefix* variant.
    if name.starts_with('~') {
        return false;
    }
    // OS / desktop-environment metadata that doesn't start with a dot.
    // `Icon\r` is the macOS Finder "custom folder icon" file — the `\r`
    // (0x0D) at the end is part of the literal name. Not a duplicate of
    // `Icon\u{d}`; that *was* the duplicate, removed for clippy.
    //
    // Case-insensitive: NTFS is case-preserving but
    // case-insensitive, so a Windows checkout / copy / Git Bash session
    // might store `THUMBS.DB` or `Desktop.ini` — same junk content,
    // different bytes. The editor-suffix check below is already
    // case-insensitive; this match brings the desktop-metadata case
    // into line.
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
