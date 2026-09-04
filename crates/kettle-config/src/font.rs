//! Bundled JetBrains Mono Nerd Font faces, embedded so AstroNvim/Neovim icons
//! render out of the box with zero user setup.

pub const FAMILY: &str = "JetBrainsMono Nerd Font";

pub static REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/JetBrainsMonoNerdFont-Regular.ttf");
pub static BOLD: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMonoNerdFont-Bold.ttf");
pub static ITALIC: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMonoNerdFont-Italic.ttf");
pub static BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/JetBrainsMonoNerdFont-BoldItalic.ttf");

pub fn all() -> [&'static [u8]; 4] {
    [REGULAR, BOLD, ITALIC, BOLD_ITALIC]
}
