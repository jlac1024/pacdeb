// SPDX-License-Identifier: AGPL-3.0-or-later
//! Running the pacdeb command line tool (and pkexec) from the GUI, with its output
//! shown as it comes. Commands that would install instead write the built packages
//! to a list, so the GUI can confirm and install them itself.

use std::cell::RefCell;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::Ctx;

/// What a finished command left behind.
pub struct Done {
    pub ok: bool,
    pub output: String,
    /// Packages the command wanted installed.
    pub packages: Vec<PathBuf>,
}

enum Msg {
    Line(String),
    Finished(bool),
}

/// `pacdeb-gui --record <list> <packages...>`: called in place of `sudo pacman -U`.
pub fn record(args: &[String]) -> ExitCode {
    let Some((list, pkgs)) = args.split_first() else {
        return ExitCode::from(2);
    };
    let mut text = fs::read_to_string(list).unwrap_or_default();
    for p in pkgs {
        text.push_str(p);
        text.push('\n');
    }
    match fs::write(list, text) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// The command line tool: PACDEB_BIN, or `pacdeb` next to this program.
pub fn cli() -> PathBuf {
    if let Some(p) = std::env::var_os("PACDEB_BIN").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("pacdeb"))).filter(|p| p.exists()).unwrap_or_else(|| "pacdeb".into())
}

fn new_record_list() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("pacdeb-gui-{}-{}.list", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// Starts `program args`, sending its output lines and then whether it succeeded.
fn spawn(program: &Path, args: &[String], record: Option<&Path>) -> Receiver<Msg> {
    let (tx, rx) = mpsc::channel();
    let mut cmd = Command::new(program);
    cmd.args(args).env("NO_COLOR", "1").env("PACDEB_PROGRESS", "lines").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(list) = record {
        let me = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "pacdeb-gui".into());
        cmd.env("PACDEB_INSTALL_CMD", format!("{me} --record {}", list.display()));
    }
    let shown = program.display().to_string();
    std::thread::spawn(move || {
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Msg::Line(format!("cannot run {shown}: {e}")));
                let _ = tx.send(Msg::Finished(false));
                return;
            }
        };
        let readers: Vec<_> = [child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>)]
            .into_iter()
            .flatten()
            .map(|r| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(r).lines().map_while(std::result::Result::ok) {
                        let _ = tx.send(Msg::Line(line));
                    }
                })
            })
            .collect();
        for r in readers {
            let _ = r.join();
        }
        let ok = child.wait().is_ok_and(|s| s.success());
        let _ = tx.send(Msg::Finished(ok));
    });
    rx
}

/// Feeds `rx` to `line` on the main loop and calls `done` once with the result.
fn pump(rx: Receiver<Msg>, line: impl Fn(&str) + 'static, done: impl FnOnce(bool, String) + 'static) {
    let output = Rc::new(RefCell::new(String::new()));
    let done = RefCell::new(Some(done));
    glib::timeout_add_local(Duration::from_millis(50), move || loop {
        match rx.try_recv() {
            Ok(Msg::Line(l)) => {
                line(&l);
                if !l.starts_with("@progress") {
                    let mut o = output.borrow_mut();
                    o.push_str(&l);
                    o.push('\n');
                }
            }
            Ok(Msg::Finished(ok)) => {
                if let Some(d) = done.borrow_mut().take() {
                    d(ok, output.borrow().clone());
                }
                return glib::ControlFlow::Break;
            }
            Err(TryRecvError::Empty) => return glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => {
                if let Some(d) = done.borrow_mut().take() {
                    d(false, output.borrow().clone());
                }
                return glib::ControlFlow::Break;
            }
        }
    });
}

fn read_list(list: &Path) -> Vec<PathBuf> {
    let pkgs = fs::read_to_string(list).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
    let _ = fs::remove_file(list);
    pkgs
}

/// Runs the command line tool without showing anything; `done` gets the result.
pub fn quiet(args: &[&str], done: impl FnOnce(Done) + 'static) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    pump(spawn(&cli(), &args, None), |_| {}, move |ok, output| done(Done { ok, output, packages: Vec::new() }));
}

/// Runs a command in a dialog that shows its output as it comes. With `collect`, the
/// packages it would install are gathered for the GUI's own install step instead.
pub fn logged(ctx: &Rc<Ctx>, title: &str, program: &Path, args: &[String], collect: bool, done: impl FnOnce(&adw::Dialog, Done) + 'static) {
    let buffer = gtk::TextBuffer::new(None);
    let text = gtk::TextView::builder()
        .buffer(&buffer)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    let scroll = gtk::ScrolledWindow::builder().child(&text).vexpand(true).min_content_height(360).build();
    let spinner = gtk::Spinner::builder().spinning(true).build();
    let status = gtk::Label::builder().label("Working...").xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::Middle).build();
    let row = gtk::Box::builder().spacing(12).build();
    row.append(&spinner);
    row.append(&status);
    let bar = gtk::ProgressBar::builder().visible(false).build();
    let bottom = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_top(8).margin_bottom(10).margin_start(12).margin_end(12).build();
    bottom.append(&row);
    bottom.append(&bar);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&scroll));
    view.add_bottom_bar(&bottom);
    let dialog = adw::Dialog::builder().title(title).content_width(720).content_height(480).child(&view).can_close(false).build();
    dialog.present(Some(&ctx.window));

    let list = collect.then(new_record_list);
    let rx = spawn(program, args, list.as_deref());
    // A step with no known size (makepkg compressing) keeps the bar moving.
    let unmeasured = Rc::new(std::cell::Cell::new(false));
    glib::timeout_add_local(Duration::from_millis(150), {
        let (bar, unmeasured, dialog) = (bar.clone(), unmeasured.clone(), dialog.clone());
        move || {
            if dialog.can_close() {
                return glib::ControlFlow::Break;
            }
            if unmeasured.get() {
                bar.pulse();
            }
            glib::ControlFlow::Continue
        }
    });
    let line = {
        let buffer = buffer.clone();
        let text = text.clone();
        let (bar, status, unmeasured) = (bar.clone(), status.clone(), unmeasured.clone());
        move |l: &str| {
            match pacdeb::progress::parse_line(l) {
                Some(pacdeb::progress::Line::Step { label, done, total }) => {
                    bar.set_visible(true);
                    status.set_label(&label);
                    unmeasured.set(total == 0);
                    if total > 0 {
                        bar.set_fraction((done as f64 / total as f64).clamp(0.0, 1.0));
                        bar.set_text(Some(&format!("{} of {}", pacdeb::human::size(done), pacdeb::human::size(total))));
                        bar.set_show_text(true);
                    } else {
                        bar.set_show_text(false);
                    }
                    return;
                }
                Some(pacdeb::progress::Line::Done { .. }) => {
                    unmeasured.set(false);
                    bar.set_visible(false);
                    status.set_label("Working...");
                    return;
                }
                None => {}
            }
            buffer.insert(&mut buffer.end_iter(), &format!("{l}\n"));
            let mark = buffer.create_mark(None, &buffer.end_iter(), false);
            text.scroll_mark_onscreen(&mark);
            buffer.delete_mark(&mark);
        }
    };
    let finished = {
        let dialog = dialog.clone();
        move |ok: bool, output: String| {
            spinner.set_spinning(false);
            spinner.set_visible(false);
            bar.set_visible(false);
            status.set_label(if ok { "Finished" } else { "Failed; the output above says why" });
            dialog.set_can_close(true);
            let packages = list.as_deref().map(read_list).unwrap_or_default();
            done(&dialog, Done { ok, output, packages });
        }
    };
    pump(rx, line, finished);
}

/// Runs the command line tool in a log dialog, then installs whatever it built with
/// the GUI's own confirmation. Every page is refreshed afterward.
pub fn cli_and_install(ctx: &Rc<Ctx>, title: &str, args: &[&str]) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let ctx2 = ctx.clone();
    logged(ctx, title, &cli(), &args, true, move |dialog, done| {
        ctx2.refresh();
        if done.ok && !done.packages.is_empty() {
            dialog.close();
            crate::install::confirm(&ctx2, done.packages);
        } else if done.ok {
            let last = done.output.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("Done").to_string();
            ctx2.toast(&last);
        }
    });
}
