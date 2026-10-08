//! Every package in an apt repository, searchable, each one trackable with a click.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use pacdeb::browse::{self, AptRepo};
use pacdeb::paths::Paths;
use pacdeb::sources::apt::{Listed, host_arch};

use crate::sources_page::plain_row;
use crate::{Ctx, run};

/// Rows shown at once; big repositories are narrowed down by searching.
const SHOWN: usize = 200;

pub struct Target {
    pub repo: AptRepo,
    pub key: PathBuf,
    /// Set when the person pinned the key's fingerprint, passed on when tracking.
    pub fingerprint: Option<String>,
    /// Packages from this repository that are already tracked.
    pub tracked: Vec<String>,
}

struct State {
    target: Target,
    all: RefCell<Vec<Listed>>,
    tracked: RefCell<Vec<String>>,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
}

pub fn open(ctx: &Rc<Ctx>, target: Target) {
    let search = gtk::SearchEntry::builder().placeholder_text("Search packages").hexpand(true).build();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let clamp = adw::Clamp::builder().maximum_size(760).child(&list).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
    let scroll = gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build();
    let spinner = gtk::Spinner::builder().spinning(true).width_request(32).height_request(32).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    let failed = adw::StatusPage::builder().icon_name("dialog-error-symbolic").title("Could not read the repository").build();
    let stack = gtk::Stack::new();
    stack.add_named(&spinner, Some("loading"));
    stack.add_named(&scroll, Some("list"));
    stack.add_named(&failed, Some("failed"));

    let title = adw::WindowTitle::new("Packages", &target.repo.repo);
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let search_bar = gtk::Box::builder().margin_start(12).margin_end(12).margin_bottom(6).build();
    search_bar.append(&search);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&search_bar);
    view.set_content(Some(&stack));
    let dialog = adw::Dialog::builder().title("Packages").content_width(760).content_height(620).child(&view).build();
    dialog.present(Some(&ctx.window));

    let tracked = target.tracked.clone();
    let state = Rc::new(State { target, all: RefCell::new(Vec::new()), tracked: RefCell::new(tracked), list, search: search.clone() });
    search.connect_search_changed({
        let (ctx, state) = (ctx.clone(), state.clone());
        move |_| render(&ctx, &state)
    });

    let (repo, key) = (state.target.repo.clone(), state.target.key.clone());
    let (ctx, state2) = (ctx.clone(), state.clone());
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            let paths = Paths::from_env().map_err(|e| e.to_string())?;
            browse::packages(&repo, &key, &paths).map_err(|e| e.to_string())
        })
        .await;
        match result {
            Ok(Ok(all)) => {
                title.set_subtitle(&format!("{} · {} packages", state2.target.repo.repo, all.len()));
                *state2.all.borrow_mut() = all;
                render(&ctx, &state2);
                stack.set_visible_child_name("list");
            }
            Ok(Err(e)) => {
                failed.set_description(Some(&glib::markup_escape_text(&e)));
                stack.set_visible_child_name("failed");
            }
            Err(_) => stack.set_visible_child_name("failed"),
        }
    });
}

fn render(ctx: &Rc<Ctx>, state: &Rc<State>) {
    let list = &state.list;
    list.remove_all();
    let query = state.search.text().to_lowercase();
    let all = state.all.borrow();
    let matches: Vec<&Listed> = all.iter().filter(|l| query.is_empty() || l.name.to_lowercase().contains(&query) || l.summary.to_lowercase().contains(&query)).collect();
    for l in matches.iter().take(SHOWN) {
        let row = plain_row(&l.name, &format!("{} · {}", l.version, l.summary));
        if state.tracked.borrow().contains(&l.name) {
            row.add_suffix(&gtk::Label::builder().label("Tracked").css_classes(["dim-label"]).build());
        } else {
            let b = gtk::Button::builder().label("Track").valign(gtk::Align::Center).build();
            let (ctx, state, name) = (ctx.clone(), state.clone(), l.name.clone());
            b.connect_clicked(move |_| track(&ctx, &state, &name));
            row.add_suffix(&b);
        }
        list.append(&row);
    }
    let hidden = matches.len().saturating_sub(SHOWN);
    if hidden > 0 {
        list.append(&adw::ActionRow::builder().title(format!("{hidden} more; search to narrow the list")).css_classes(["dim-label"]).build());
    } else if matches.is_empty() {
        list.append(&adw::ActionRow::builder().title("No package matches").css_classes(["dim-label"]).build());
    }
}

fn track(ctx: &Rc<Ctx>, state: &Rc<State>, name: &str) {
    let t = &state.target;
    let mut args: Vec<String> = ["add", name, "--source", "apt", "--repo", &t.repo.repo, "--suite", &t.repo.suite, "--component", &t.repo.component]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if t.repo.arch != host_arch() {
        args.extend(["--arch".into(), t.repo.arch.clone()]);
    }
    args.extend(["--key".into(), t.key.display().to_string()]);
    if let Some(f) = t.fingerprint.as_ref().and_then(|f| f.split_whitespace().next()) {
        args.extend(["--key-fingerprint".into(), f.to_string()]);
    }
    let (ctx2, state, name) = (ctx.clone(), state.clone(), name.to_string());
    run::logged(ctx, &format!("Tracking {name}"), &run::cli(), &args, false, move |log, done| {
        if done.ok {
            log.close();
            state.tracked.borrow_mut().push(name.clone());
            render(&ctx2, &state);
            ctx2.refresh();
            ctx2.toast(&format!("Tracking {name}; install it from the Apps page with Update"));
        }
    });
}
