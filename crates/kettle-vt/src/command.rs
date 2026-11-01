//! Bounded shell command-correlation metadata.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandToken([u8; 32]);

impl CommandToken {
    pub fn parse(value: &[u8]) -> Option<Self> {
        let bytes: [u8; 32] = value.try_into().ok()?;
        bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            .then_some(Self(bytes))
    }

    pub fn as_str(&self) -> &str {
        // Construction accepts ASCII hexadecimal bytes only.
        std::str::from_utf8(&self.0).expect("validated command token")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandShell {
    Bash = 1,
    Zsh = 2,
    Fish = 3,
    PowerShell = 4,
}

impl CommandShell {
    pub fn parse(value: &[u8]) -> Option<Self> {
        match value {
            b"bash" => Some(Self::Bash),
            b"zsh" => Some(Self::Zsh),
            b"fish" => Some(Self::Fish),
            b"powershell" => Some(Self::PowerShell),
            _ => None,
        }
    }

    pub fn from_code(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Bash),
            2 => Some(Self::Zsh),
            3 => Some(Self::Fish),
            4 => Some(Self::PowerShell),
            _ => None,
        }
    }
}
