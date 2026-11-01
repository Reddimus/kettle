use std::ops::{Index, IndexMut};

use crate::vte::ansi::{NamedColor, Rgb};

/// Number of terminal colors.
pub const COUNT: usize =
    crate::vte::ansi::SPECIAL_COLOR_BASE + crate::vte::ansi::SPECIAL_COLOR_COUNT;

/// Array of indexed colors.
///
/// | Indices  | Description       |
/// | -------- | ----------------- |
/// | 0..16    | Named ANSI colors |
/// | 16..232  | Color cube        |
/// | 233..256 | Grayscale ramp    |
/// | 256      | Foreground        |
/// | 257      | Background        |
/// | 258      | Cursor            |
/// | 259..267 | Dim colors        |
/// | 267      | Bright foreground |
/// | 268      | Dim foreground    |
/// | 269..274 | Special attribute colors |
#[derive(Copy, Clone)]
pub struct Colors([Option<Rgb>; COUNT], u8);

impl Default for Colors {
    fn default() -> Self {
        Self([None; COUNT], 0)
    }
}

impl Colors {
    #[inline]
    pub fn has_special_modes(&self) -> bool {
        self.1 & 0x3f != 0
    }

    #[inline]
    pub fn special_mode(&self, index: usize) -> bool {
        index < 6 && self.1 & (1 << index) != 0
    }

    pub fn set_special_mode(&mut self, index: usize, enabled: bool) -> bool {
        if index >= 6 {
            return false;
        }
        let before = self.1;
        if enabled {
            self.1 |= 1 << index;
        } else {
            self.1 &= !(1 << index);
        }
        self.1 != before
    }

    #[inline]
    pub fn reverse_video(&self) -> bool {
        self.1 & 0x40 != 0
    }

    pub fn set_reverse_video(&mut self, enabled: bool) -> bool {
        let before = self.1;
        if enabled {
            self.1 |= 0x40;
        } else {
            self.1 &= !0x40;
        }
        self.1 != before
    }

    /// Canonical storage for a displayed palette or dynamic-colour slot.
    #[inline]
    pub fn mapped_index(&self, index: usize) -> usize {
        if !self.reverse_video() {
            return index;
        }
        match index {
            0 => 7,
            7 => 0,
            8 => 15,
            15 => 8,
            256 => 257,
            257 => 256,
            _ => index,
        }
    }
}

impl Index<usize> for Colors {
    type Output = Option<Rgb>;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.0[self.mapped_index(index)]
    }
}

impl IndexMut<usize> for Colors {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let index = self.mapped_index(index);
        &mut self.0[index]
    }
}

impl Index<NamedColor> for Colors {
    type Output = Option<Rgb>;

    #[inline]
    fn index(&self, index: NamedColor) -> &Self::Output {
        &self[index as usize]
    }
}

impl IndexMut<NamedColor> for Colors {
    #[inline]
    fn index_mut(&mut self, index: NamedColor) -> &mut Self::Output {
        &mut self[index as usize]
    }
}
