// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fmt;

/// A plain error message. pacdeb reports errors to a person, so a readable chain of
/// context is more useful than typed variants.
#[derive(Debug)]
pub struct Error(String);

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(msg: impl Into<String>) -> Self {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(plain(&e))
    }
}

/// An error's message without the " (os error 2)" style suffix, which means
/// nothing to the person reading it.
pub fn plain(e: &impl fmt::Display) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) if s.ends_with(')') => s[..i].to_string(),
        _ => s,
    }
}

pub trait Context<T> {
    fn context(self, msg: impl fmt::Display) -> Result<T>;
}

impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, msg: impl fmt::Display) -> Result<T> {
        self.map_err(|e| Error(format!("{msg}: {}", plain(&e))))
    }
}

macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::error::Error::new(format!($($arg)*)))
    };
}
pub(crate) use bail;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_os_error_codes() {
        let cases = [
            ("No such file or directory (os error 2)", "No such file or directory"),
            ("Permission denied (os error 13)", "Permission denied"),
            ("plain message", "plain message"),
            ("keeps (other) parentheses", "keeps (other) parentheses"),
        ];
        for (input, want) in cases {
            assert_eq!(plain(&input), want, "{input}");
        }
    }
}
