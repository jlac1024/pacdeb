//! Terminal colors. Only used when stdout is a terminal and NO_COLOR is not set, so
//! piped output and logs stay plain.

use std::io::IsTerminal;

#[derive(Debug, Clone, Copy)]
pub struct Style {
    on: bool,
}

impl Style {
    pub fn for_stdout() -> Style {
        Style { on: color_allowed() && std::io::stdout().is_terminal() }
    }

    pub fn for_stderr() -> Style {
        Style { on: color_allowed() && std::io::stderr().is_terminal() }
    }

    #[cfg(test)]
    pub fn plain() -> Style {
        Style { on: false }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.on { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }

    /// Section titles that need attention.
    pub fn warn(&self, s: &str) -> String {
        self.paint("1;33", s)
    }

    pub fn bad(&self, s: &str) -> String {
        self.paint("1;31", s)
    }

    pub fn good(&self, s: &str) -> String {
        self.paint("1;32", s)
    }

    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }

    /// Secondary detail such as line numbers and reasons.
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
}

fn color_allowed() -> bool {
    !std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paints_only_when_on() {
        assert_eq!(Style::plain().warn("x"), "x");
        assert_eq!(Style { on: true }.warn("x"), "\x1b[1;33mx\x1b[0m");
        assert_eq!(Style { on: true }.dim("x"), "\x1b[2mx\x1b[0m");
    }
}
