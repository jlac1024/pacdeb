// SPDX-License-Identifier: AGPL-3.0-or-later
//! Progress for long steps (downloads, unpacking, packing). In a terminal it is a bar
//! redrawn in place on stderr. With PACDEB_PROGRESS=lines it is machine readable lines
//! for pacdeb-gui:
//!   @progress<TAB>label<TAB>done<TAB>total   (total 0 when unknown)
//!   @progress-done<TAB>label
//! Otherwise (logs, pipes) it says nothing.

use std::io::{IsTerminal, Read, Write};
use std::time::{Duration, Instant};

use crate::human;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Bar,
    Lines,
    Off,
}

fn mode() -> Mode {
    match std::env::var("PACDEB_PROGRESS").as_deref() {
        Ok("lines") => Mode::Lines,
        Ok("off") => Mode::Off,
        _ if std::io::stderr().is_terminal() => Mode::Bar,
        _ => Mode::Off,
    }
}

const REDRAW: Duration = Duration::from_millis(100);
const BAR_WIDTH: usize = 24;

pub struct Progress {
    label: String,
    /// A status line rather than a bar: shown while the step runs, cleared after.
    transient: bool,
    total: u64,
    done: u64,
    mode: Mode,
    started: Instant,
    drawn: Option<Instant>,
}

impl Progress {
    /// `total` in bytes; None or 0 when unknown.
    pub fn new(label: impl Into<String>, total: Option<u64>) -> Progress {
        let p = Progress { label: label.into(), transient: false, total: total.unwrap_or(0), done: 0, mode: mode(), started: Instant::now(), drawn: None };
        p.emit();
        p
    }

    /// A step whose progress cannot be measured (another program doing the work).
    pub fn busy(label: impl Into<String>) -> Progress {
        // A terminal shows the other program's own output; no bar is drawn there.
        let mode = match mode() {
            Mode::Bar => Mode::Off,
            m => m,
        };
        let p = Progress { label: label.into(), transient: false, total: 0, done: 0, mode, started: Instant::now(), drawn: None };
        p.emit();
        p
    }

    /// A step pacdeb does itself but cannot measure, like reading a deb: "label..." in a
    /// terminal until it is done, then the line is cleared.
    pub fn status(label: impl Into<String>) -> Progress {
        let mut p = Progress { label: label.into(), transient: true, total: 0, done: 0, mode: mode(), started: Instant::now(), drawn: None };
        p.emit();
        p.drawn = Some(Instant::now());
        p
    }

    pub fn add(&mut self, n: u64) {
        self.done += n;
        if self.drawn.is_none_or(|t| t.elapsed() >= REDRAW) {
            self.emit();
            self.drawn = Some(Instant::now());
        }
    }

    pub fn finish(mut self) {
        if self.total > 0 {
            self.done = self.total;
        }
        match self.mode {
            Mode::Bar if self.transient => eprint!("\r\x1b[K"),
            Mode::Bar => {
                self.emit();
                eprintln!();
            }
            Mode::Lines => eprintln!("@progress-done\t{}", self.label),
            Mode::Off => {}
        }
        self.mode = Mode::Off;
    }

    fn emit(&self) {
        match self.mode {
            Mode::Bar => {
                let text = if self.transient { format!("{}...", self.label) } else { render(&self.label, self.done, self.total, self.started.elapsed()) };
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "\r\x1b[K{text}");
                let _ = err.flush();
            }
            Mode::Lines => eprintln!("@progress\t{}\t{}\t{}", self.label, self.done, self.total),
            Mode::Off => {}
        }
    }
}

impl Drop for Progress {
    /// A step that failed part way leaves the terminal on a fresh line.
    fn drop(&mut self) {
        match self.mode {
            Mode::Bar if self.transient => eprint!("\r\x1b[K"),
            Mode::Bar if self.drawn.is_some() => eprintln!(),
            Mode::Lines => eprintln!("@progress-done\t{}", self.label),
            _ => {}
        }
    }
}

/// "label  [=======>       ]  45%  93.4 MiB / 207.7 MiB  12.1 MiB/s"
fn render(label: &str, done: u64, total: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    let rate = if secs >= 0.5 { format!("  {}/s", human::size((done as f64 / secs) as u64)) } else { String::new() };
    if total == 0 {
        return format!("{label}  {}{rate}", human::size(done));
    }
    let frac = (done as f64 / total as f64).clamp(0.0, 1.0);
    let filled = (frac * BAR_WIDTH as f64).round() as usize;
    let bar = match filled {
        0 => " ".repeat(BAR_WIDTH),
        f if f >= BAR_WIDTH => "=".repeat(BAR_WIDTH),
        f => format!("{}>{}", "=".repeat(f - 1), " ".repeat(BAR_WIDTH - f)),
    };
    // Rounded down, so 100% only shows once everything is done.
    format!("{label}  [{bar}] {:>3}%  {} / {}{rate}", (frac * 100.0).floor() as u64, human::size(done), human::size(total))
}

/// Counts what is read through it.
pub struct Counting<'a, R> {
    pub inner: R,
    pub progress: &'a mut Progress,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.progress.add(n as u64);
        Ok(n)
    }
}

/// A progress line from PACDEB_PROGRESS=lines output: (label, done, total), or the
/// label alone when the step finished.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Step { label: String, done: u64, total: u64 },
    Done { label: String },
}

pub fn parse_line(line: &str) -> Option<Line> {
    let mut parts = line.split('\t');
    match parts.next()? {
        "@progress" => {
            let label = parts.next()?.to_string();
            let done = parts.next()?.parse().ok()?;
            let total = parts.next()?.parse().ok()?;
            Some(Line::Step { label, done, total })
        }
        "@progress-done" => Some(Line::Done { label: parts.next()?.to_string() }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_bars() {
        let cases = [
            (0, 100, 0, "x  [                        ]   0%  0 B / 100 B"),
            (50, 100, 0, "x  [===========>            ]  50%  50 B / 100 B"),
            (100, 100, 0, "x  [========================] 100%  100 B / 100 B"),
            (300, 0, 0, "x  300 B"),
            (2048, 4096, 2, "x  [===========>            ]  50%  2.0 KiB / 4.0 KiB  1.0 KiB/s"),
        ];
        for (done, total, secs, want) in cases {
            assert_eq!(render("x", done, total, Duration::from_secs(secs)), want, "{done}/{total}");
        }
    }

    #[test]
    fn reads_progress_lines() {
        let cases = [
            ("@progress\tDownloading a.deb\t10\t200", Some(Line::Step { label: "Downloading a.deb".into(), done: 10, total: 200 })),
            ("@progress-done\tDownloading a.deb", Some(Line::Done { label: "Downloading a.deb".into() })),
            ("@progress\tbroken", None),
            ("Building app", None),
        ];
        for (line, want) in cases {
            assert_eq!(parse_line(line), want, "{line}");
        }
    }
}
