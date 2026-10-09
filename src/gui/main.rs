// SPDX-License-Identifier: AGPL-3.0-or-later
//! pacdeb-gui: a window over pacdeb. It reads apps and versions through the library and
//! runs the pacdeb command line tool for anything that changes something, so both
//! behave the same. Installs go through its own confirmation and a polkit prompt.

mod apps_page;
mod browse_dialog;
mod convert_page;
mod install;
mod repo_dialog;
mod search_page;
mod run;
mod settings_page;
mod source_dialog;
mod sources_page;

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;

use adw::prelude::*;

const APP_ID: &str = "local.pacdeb.Gui";

/// What every page needs: the window to put dialogs on, a place for short messages,
/// and a way to tell the other pages that something changed.
pub struct Ctx {
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    refreshers: RefCell<Vec<Rc<dyn Fn()>>>,
}

impl Ctx {
    pub fn toast(&self, msg: &str) {
        let t = adw::Toast::new(msg);
        t.set_timeout(5);
        self.toasts.add_toast(t);
    }

    pub fn on_refresh(&self, f: impl Fn() + 'static) {
        self.refreshers.borrow_mut().push(Rc::new(f));
    }

    /// Reloads every page from the config and state files.
    pub fn refresh(&self) {
        let all: Vec<Rc<dyn Fn()>> = self.refreshers.borrow().clone();
        for f in all {
            f();
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // When the command line tool installs on the GUI's behalf it calls back with the
    // built packages; they are written down for the GUI's own confirmation step.
    if args.get(1).map(String::as_str) == Some("--record") {
        return run::record(&args[2..]);
    }
    let mut page = None;
    let mut deb = None;
    let mut search = None;
    let mut rest = args[1..].iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--page" => page = rest.next().cloned(),
            "--search" => search = rest.next().cloned(),
            "-V" | "--version" => {
                println!("pacdeb-gui {}", pacdeb::version());
                println!("License AGPL-3.0-or-later: free software, with no warranty. See the LICENSE file.");
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                println!("Usage: pacdeb-gui [--page apps|search|sources|convert|settings] [--search <words>] [--version] [file.deb]");
                return ExitCode::SUCCESS;
            }
            _ if !a.starts_with('-') => deb = Some(std::path::PathBuf::from(a)),
            other => {
                eprintln!("pacdeb-gui: unknown option '{other}'. Usage: pacdeb-gui [--page apps|search|sources|convert|settings] [--search <words>] [--version] [file.deb]");
                return ExitCode::from(2);
            }
        }
    }
    let app = adw::Application::builder().application_id(APP_ID).build();
    // Off-screen test runs must not hand over to a pacdeb window already open on the desktop.
    #[cfg(debug_assertions)]
    if std::env::var_os("PACDEB_GUI_SNAPSHOT").is_some() {
        app.set_flags(gtk::gio::ApplicationFlags::NON_UNIQUE);
    }
    app.connect_activate(move |app| build_window(app, page.as_deref(), deb.clone(), search.clone()));
    let code = app.run_with_args(&args[..1]);
    code.into()
}

fn build_window(app: &adw::Application, page: Option<&str>, deb: Option<std::path::PathBuf>, search: Option<String>) {
    if let Some(w) = app.active_window() {
        w.present();
        return;
    }
    let stack = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
    let menu = gtk::gio::Menu::new();
    menu.append(Some("About pacdeb"), Some("app.about"));
    header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).tooltip_text("Menu").build());
    let toasts = adw::ToastOverlay::new();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&stack));
    toasts.set_child(Some(&view));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("pacdeb")
        .default_width(860)
        .default_height(640)
        .content(&toasts)
        .build();
    let ctx = Rc::new(Ctx { window: window.clone(), toasts, refreshers: RefCell::new(Vec::new()) });
    let about = gtk::gio::SimpleAction::new("about", None);
    about.connect_activate({
        let window = window.clone();
        move |_, _| show_about(&window)
    });
    app.add_action(&about);

    stack.add_titled_with_icon(&apps_page::build(&ctx), Some("apps"), "Apps", "view-list-symbolic");
    let (search_widget, search_entry) = search_page::build(&ctx);
    stack.add_titled_with_icon(&search_widget, Some("search"), "Search", "system-search-symbolic");
    stack.add_titled_with_icon(&sources_page::build(&ctx), Some("sources"), "Sources", "network-server-symbolic");
    let (convert, converter) = convert_page::build(&ctx);
    stack.add_titled_with_icon(&convert, Some("convert"), "Convert", "package-x-generic-symbolic");
    stack.add_titled_with_icon(&settings_page::build(&ctx), Some("settings"), "Settings", "emblem-system-symbolic");
    match (deb, page) {
        _ if search.is_some() => {
            stack.set_visible_child_name("search");
            search_entry.set_text(search.as_deref().unwrap_or_default());
        }
        (Some(path), _) => {
            stack.set_visible_child_name("convert");
            convert_page::show(&converter, path);
        }
        (None, Some(p)) if ["apps", "search", "sources", "convert", "settings"].contains(&p) => stack.set_visible_child_name(p),
        _ => {}
    }
    window.present();
    #[cfg(debug_assertions)]
    {
        open_for_tests(&ctx);
        snapshot_for_tests(&window);
    }
}

fn show_about(window: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name("pacdeb")
        .application_icon("system-software-install")
        .version(env!("CARGO_PKG_VERSION"))
        .comments(format!("Turns Debian .deb packages into pacman packages and keeps them updated, like apt.\n\nBuild {}", pacdeb::build()))
        .website(env!("CARGO_PKG_REPOSITORY"))
        .issue_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/issues"))
        .copyright("© 2026 Jeff LaCombe (jlac1024)")
        .license_type(gtk::License::Agpl30)
        .build();
    about.present(Some(window));
}

/// Debug builds only: PACDEB_GUI_OPEN=add | edit:<app> | install:<package file> |
/// browse:<app or repository> | repo:<repository> | remove:<app> | build:<deb> |
/// details:<package/repository> opens
/// that dialog at startup, for checking it with PACDEB_GUI_SNAPSHOT.
#[cfg(debug_assertions)]
fn open_for_tests(ctx: &Rc<Ctx>) {
    let Some(what) = std::env::var("PACDEB_GUI_OPEN").ok() else {
        return;
    };
    match what.split_once(':') {
        None if what == "add" => source_dialog::open(ctx, None),
        Some(("edit", app)) => source_dialog::open(ctx, Some(app)),
        Some(("install", pkg)) => install::confirm(ctx, vec![std::path::PathBuf::from(pkg)]),
        Some(("browse", name)) => {
            let paths = pacdeb::paths::Paths::from_env().expect("paths");
            let config = pacdeb::registry::Config::load(&paths.config).expect("config");
            match pacdeb::browse::repository_of(&config, name) {
                Ok(repo) => browse_dialog::open(ctx, &repo),
                Err(e) => eprintln!("PACDEB_GUI_OPEN: {e}"),
            }
        }
        Some(("repo", name)) => repo_dialog::open(ctx, name),
        Some(("details", spec)) => search_page::show_details(ctx, spec),
        None if what == "about" => show_about(&ctx.window),
        Some(("build", deb)) => run::cli_and_install(ctx, "Building", &["convert", "--direct", "--out", "build/sandbox/out", deb]),
        Some(("remove", app)) => {
            let paths = pacdeb::paths::Paths::from_env().expect("paths");
            let config = pacdeb::registry::Config::load(&paths.config).expect("config");
            let pkg = config.apps.get(app).and_then(|a| a.pkgname.clone()).unwrap_or_else(|| app.to_string());
            apps_page::confirm_remove(ctx, app, &pkg);
        }
        _ => eprintln!("PACDEB_GUI_OPEN: unknown value {what}"),
    }
}

/// Debug builds only: with PACDEB_GUI_SNAPSHOT=<file.png>:<seconds>, the window saves a
/// picture of itself after that many seconds and quits, so changes can be checked
/// without a person at the screen.
#[cfg(debug_assertions)]
fn snapshot_for_tests(window: &adw::ApplicationWindow) {
    let Some(spec) = std::env::var("PACDEB_GUI_SNAPSHOT").ok() else {
        return;
    };
    let (file, secs) = spec.rsplit_once(':').map_or((spec.as_str(), 3), |(f, s)| (f, s.parse().unwrap_or(3)));
    let (file, window) = (file.to_string(), window.clone());
    gtk::glib::timeout_add_seconds_local_once(secs, move || {
        // An open dialog is what is being checked; otherwise the window's content.
        let Some(content) = window.visible_dialog().map(|d| d.upcast::<gtk::Widget>()).or_else(|| window.content()) else {
            return;
        };
        let paintable = gtk::WidgetPaintable::new(Some(&content));
        let snapshot = gtk::Snapshot::new();
        // Pages draw on the window's background, which is not part of the content.
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, content.width() as f32, content.height() as f32);
        snapshot.append_color(&gtk::gdk::RGBA::new(0.13, 0.13, 0.15, 1.0), &bounds);
        paintable.snapshot(&snapshot, content.width() as f64, content.height() as f64);
        match (snapshot.to_node(), window.renderer()) {
            (Some(node), Some(renderer)) => {
                if let Err(e) = renderer.render_texture(&node, None).save_to_png(&file) {
                    eprintln!("snapshot: {e}");
                }
            }
            (node, renderer) => eprintln!("snapshot: nothing to save (content: {}, renderer: {})", node.is_some(), renderer.is_some()),
        }
        if let Some(app) = window.application() {
            app.quit();
        }
    });
}
