// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Convert page: open or drop a .deb, read what converting it would do, then build
//! it or build and install it. Installing an untracked deb starts tracking it, as
//! `pacdeb install` does.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::{Ctx, run};

pub struct Page {
    stack: gtk::Stack,
    title: adw::WindowTitle,
    report: gtk::TextBuffer,
    actions: gtk::Box,
    file: RefCell<Option<PathBuf>>,
}

pub fn build(ctx: &Rc<Ctx>) -> (gtk::Widget, Rc<Page>) {
    let open = gtk::Button::builder().label("Open a .deb").halign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
    let empty = adw::StatusPage::builder()
        .icon_name("package-x-generic-symbolic")
        .title("Convert a .deb")
        .description("Open a .deb file or drop one here to see what pacdeb would build from it")
        .child(&open)
        .build();

    let report = gtk::TextBuffer::new(None);
    let view = gtk::TextView::builder()
        .buffer(&report)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    let scroll = gtk::ScrolledWindow::builder().child(&view).vexpand(true).build();
    let title = adw::WindowTitle::new("", "");
    let other = gtk::Button::builder().icon_name("document-open-symbolic").tooltip_text("Open another .deb").css_classes(["flat"]).build();
    let build_only = gtk::Button::with_label("Build package");
    let install = gtk::Button::builder().label("Build and install").css_classes(["suggested-action"]).build();
    let actions = gtk::Box::builder().spacing(8).build();
    actions.append(&build_only);
    actions.append(&install);
    let bar = gtk::Box::builder().spacing(8).margin_top(8).margin_bottom(8).margin_start(12).margin_end(12).build();
    bar.append(&other);
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Start);
    bar.append(&title);
    bar.append(&actions);
    let shown = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shown.append(&bar);
    shown.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    shown.append(&scroll);

    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&shown, Some("report"));
    let page = Rc::new(Page { stack: stack.clone(), title, report, actions, file: RefCell::new(None) });

    for button in [&open, &other] {
        let (ctx, page) = (ctx.clone(), page.clone());
        button.connect_clicked(move |_| choose(&ctx, &page));
    }
    build_only.connect_clicked({
        let (ctx, page) = (ctx.clone(), page.clone());
        move |_| {
            if let Some(f) = page.file.borrow().as_ref() {
                run::cli_and_install(&ctx, "Building", &["convert", &f.display().to_string()]);
            }
        }
    });
    install.connect_clicked({
        let (ctx, page) = (ctx.clone(), page.clone());
        move |_| {
            if let Some(f) = page.file.borrow().as_ref() {
                run::cli_and_install(&ctx, "Building", &["install", &f.display().to_string()]);
            }
        }
    });

    let drop = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
    drop.connect_drop({
        let page = page.clone();
        move |_, value, _, _| match value.get::<gio::File>().ok().and_then(|f| f.path()) {
            Some(path) => {
                show(&page, path);
                true
            }
            None => false,
        }
    });
    stack.add_controller(drop);
    (stack.upcast(), page)
}

fn choose(ctx: &Rc<Ctx>, page: &Rc<Page>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Debian packages"));
    filter.add_pattern("*.deb");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title("Open a .deb").filters(&filters).modal(true).build();
    let (window, page) = (ctx.window.clone(), page.clone());
    glib::spawn_future_local(async move {
        if let Ok(file) = dialog.open_future(Some(&window)).await {
            if let Some(path) = file.path() {
                show(&page, path);
            }
        }
    });
}

/// Shows the dry run report for `path`.
pub fn show(page: &Rc<Page>, path: PathBuf) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    page.title.set_title(&name);
    page.title.set_subtitle(&path.parent().map(|p| p.display().to_string()).unwrap_or_default());
    page.report.set_text("Reading the package...");
    page.actions.set_sensitive(false);
    page.stack.set_visible_child_name("report");
    *page.file.borrow_mut() = Some(path.clone());
    let page = page.clone();
    run::quiet(&["convert", "--dry-run", &path.display().to_string()], move |done| {
        // Only the newest file's report counts if several were opened quickly.
        if page.file.borrow().as_ref() != Some(&path) {
            return;
        }
        page.report.set_text(&done.output);
        page.actions.set_sensitive(done.ok);
    });
}
