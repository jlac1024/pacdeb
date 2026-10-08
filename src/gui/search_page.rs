//! The Search tab: every package in the saved apt repositories (from the lists the last
//! Update fetched) and the built in apps, each installable with one click, like
//! 'pacdeb search' and 'pacdeb install'.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use pacdeb::aptrepos::{self, Found};
use pacdeb::paths::Paths;
use pacdeb::registry::{Config, presets};

use crate::sources_page::plain_row;
use crate::{Ctx, run};

/// Results shown at once.
const SHOWN: usize = 200;

struct Page {
    entry: gtk::SearchEntry,
    list: gtk::ListBox,
    status: gtk::Label,
    /// Every package, read once and again after anything changes.
    all: RefCell<Option<Vec<Found>>>,
    loading: RefCell<bool>,
}

pub fn build(ctx: &Rc<Ctx>) -> (gtk::Widget, gtk::SearchEntry) {
    let entry = gtk::SearchEntry::builder().placeholder_text("Search packages, e.g. code or editor").hexpand(true).build();
    let status = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).wrap(true).build();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(12).margin_end(12).build();
    content.append(&entry);
    content.append(&status);
    content.append(&list);
    let clamp = adw::Clamp::builder().maximum_size(800).child(&content).build();
    let scroll = gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build();

    let page = Rc::new(Page { entry: entry.clone(), list, status, all: RefCell::new(None), loading: RefCell::new(false) });
    render(ctx, &page);
    entry.connect_search_changed({
        let (ctx, page) = (ctx.clone(), page.clone());
        move |_| render(&ctx, &page)
    });
    ctx.on_refresh({
        let (ctx, page) = (ctx.clone(), page.clone());
        move || {
            *page.all.borrow_mut() = None;
            render(&ctx, &page);
        }
    });
    (scroll.upcast(), entry)
}

/// Reads every package off the main thread, then shows the results.
fn load(ctx: &Rc<Ctx>, page: &Rc<Page>) {
    if *page.loading.borrow() {
        return;
    }
    *page.loading.borrow_mut() = true;
    page.status.set_label("Reading the package lists...");
    let (ctx, page) = (ctx.clone(), page.clone());
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(|| {
            let paths = Paths::from_env().ok()?;
            let config = Config::load(&paths.config).ok()?;
            Some(aptrepos::all_packages(&config, &paths))
        })
        .await;
        *page.loading.borrow_mut() = false;
        let (all, failed) = result.ok().flatten().unwrap_or_default();
        if !failed.is_empty() {
            ctx.toast(&format!("Could not read: {}", failed.join("; ")));
        }
        *page.all.borrow_mut() = Some(all);
        render(&ctx, &page);
    });
}

fn render(ctx: &Rc<Ctx>, page: &Rc<Page>) {
    let query = page.entry.text().trim().to_string();
    page.list.remove_all();
    page.list.set_visible(false);
    let config = Paths::from_env().and_then(|p| Config::load(&p.config)).unwrap_or_default();
    if query.is_empty() {
        let text = if config.apt.is_empty() {
            "Search finds packages in your saved apt repositories. None are saved yet; add one on the Sources page."
        } else {
            "Type to search the packages in your apt repositories and the built in apps. Update on the Apps page refreshes the lists."
        };
        page.status.set_label(text);
        return;
    }
    let all = page.all.borrow().clone();
    let Some(all) = all else {
        load(ctx, page);
        return;
    };
    let hits = aptrepos::search(&all, &query);
    let tracked = aptrepos::tracked_packages(&config);
    let presets: Vec<String> = presets().into_keys().filter(|p| p.contains(&query.to_lowercase()) && !config.apps.contains_key(p)).collect();
    for p in &presets {
        let row = plain_row(p, "built in app");
        row.add_suffix(&install_button(ctx, p.clone()));
        page.list.append(&row);
    }
    for f in hits.iter().take(SHOWN) {
        let row = plain_row(&f.package.name, &format!("{} · {} · {}", f.repo, f.package.version, f.package.summary));
        if tracked.contains(&(f.repo.clone(), f.package.name.clone())) {
            row.add_suffix(&gtk::Label::builder().label("Tracked").css_classes(["dim-label"]).build());
        } else {
            row.add_suffix(&install_button(ctx, format!("{}/{}", f.package.name, f.repo)));
        }
        page.list.append(&row);
    }
    let total = hits.len() + presets.len();
    page.list.set_visible(total > 0);
    page.status.set_label(&match total {
        0 => format!("Nothing matches '{query}'. Update on the Apps page refreshes the package lists."),
        n if hits.len() > SHOWN => format!("{n} matches; the first {SHOWN} are shown"),
        1 => "1 match".to_string(),
        n => format!("{n} matches"),
    });
}

/// Installs `spec` (a name, or name/repository) the way 'pacdeb install' does.
fn install_button(ctx: &Rc<Ctx>, spec: String) -> gtk::Button {
    let b = gtk::Button::builder().label("Install").valign(gtk::Align::Center).css_classes(["suggested-action"]).build();
    let ctx = ctx.clone();
    b.connect_clicked(move |_| {
        let name = spec.split('/').next().unwrap_or(&spec).to_string();
        run::cli_and_install(&ctx, &format!("Installing {name}"), &["install", &spec]);
    });
    b
}
