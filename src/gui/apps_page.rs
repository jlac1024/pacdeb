//! The Apps page: tracked apps with their versions, checking for updates, updating,
//! and adding, editing or removing apps.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use pacdeb::apps::{self, Status};
use pacdeb::paths::Paths;
use pacdeb::registry::{Config, State};

use crate::{Ctx, run, source_dialog};

type Found = Rc<RefCell<BTreeMap<String, Result<Status, String>>>>;

pub fn build(ctx: &Rc<Ctx>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Tracked apps")
        .description("Apps pacdeb builds from .deb releases and keeps updated")
        .build();
    let check = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Check for updates").css_classes(["flat"]).build();
    let update_all = gtk::Button::builder().label("Update all").css_classes(["flat"]).build();
    let add = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text("Add an app").css_classes(["flat"]).build();
    let buttons = gtk::Box::builder().spacing(6).build();
    buttons.append(&check);
    buttons.append(&update_all);
    buttons.append(&add);
    group.set_header_suffix(Some(&buttons));
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    group.add(&list);
    page.add(&group);

    let found: Found = Rc::new(RefCell::new(BTreeMap::new()));
    let fill: Rc<dyn Fn()> = {
        let (ctx, list, found) = (ctx.clone(), list.clone(), found.clone());
        Rc::new(move || fill_list(&ctx, &list, &found))
    };
    fill();
    ctx.on_refresh({
        let fill = fill.clone();
        move || fill()
    });

    check.connect_clicked({
        let (fill, found, ctx) = (fill.clone(), found.clone(), ctx.clone());
        move |button| {
            button.set_sensitive(false);
            let (fill, found, ctx, button) = (fill.clone(), found.clone(), ctx.clone(), button.clone());
            glib::spawn_future_local(async move {
                match gio::spawn_blocking(check_all).await {
                    Ok(Ok(results)) => *found.borrow_mut() = results,
                    Ok(Err(e)) => ctx.toast(&e),
                    Err(_) => ctx.toast("Checking stopped unexpectedly"),
                }
                fill();
                button.set_sensitive(true);
            });
        }
    });
    update_all.connect_clicked({
        let ctx = ctx.clone();
        move |_| run::cli_and_install(&ctx, "Updating all apps", &["update"])
    });
    add.connect_clicked({
        let ctx = ctx.clone();
        move |_| source_dialog::open(&ctx, None)
    });
    page.upcast()
}

/// Asks every app's source for its newest version. Runs off the main thread.
fn check_all() -> Result<BTreeMap<String, Result<Status, String>>, String> {
    let paths = Paths::from_env().map_err(|e| e.to_string())?;
    let config = Config::load(&paths.config).map_err(|e| e.to_string())?;
    let state = State::load(&paths.state).map_err(|e| e.to_string())?;
    Ok(config.apps.keys().map(|n| (n.clone(), apps::check_app(n, &config, &state, &paths).map_err(|e| e.to_string()))).collect())
}

fn fill_list(ctx: &Rc<Ctx>, list: &gtk::ListBox, found: &Found) {
    list.remove_all();
    let loaded = Paths::from_env().and_then(|p| Ok((Config::load(&p.config)?, State::load(&p.state)?)));
    let (config, state) = match loaded {
        Ok(cs) => cs,
        Err(e) => {
            list.append(&crate::sources_page::plain_row("Cannot read pacdeb's settings", &e.to_string()));
            return;
        }
    };
    if config.apps.is_empty() {
        list.append(&adw::ActionRow::builder().title("No apps tracked yet").subtitle("Add one with the + button, or install a .deb on the Convert page").build());
        return;
    }
    for (name, app) in &config.apps {
        let s = state.apps.get(name);
        let pkg = app.pkgname.as_deref().unwrap_or(name);
        let mut parts = vec![app.source.kind().to_string()];
        if app.source.uses_channel() {
            parts.push(format!("channel {}", config.channel(app).unwrap_or_else(|| "not set".into())));
        }
        parts.push(format!("built {}", s.and_then(|s| s.deb_version.clone()).unwrap_or_else(|| "nothing yet".into())));
        parts.push(format!("installed {}", apps::installed_version(pkg).unwrap_or_else(|| "no".into())));
        let row = adw::ActionRow::builder().title(name).subtitle(parts.join(" · ")).build();

        let (label, class, newer) = match found.borrow().get(name) {
            None => (String::new(), "dim-label", false),
            Some(Err(e)) => (format!("error: {e}"), "error", false),
            Some(Ok(Status::UpToDate(_))) => ("up to date".into(), "success", false),
            Some(Ok(Status::Newer { latest, .. })) => (format!("{latest} available"), "warning", true),
            Some(Ok(Status::Changed(true))) => ("new download".into(), "warning", true),
            Some(Ok(Status::Changed(false))) => ("unchanged".into(), "success", false),
            Some(Ok(Status::Manual)) => ("manual".into(), "dim-label", false),
        };
        if !label.is_empty() {
            let l = gtk::Label::builder().label(&label).css_classes([class]).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(36).tooltip_text(&label).build();
            row.add_suffix(&l);
        }
        if newer {
            let b = gtk::Button::builder().label("Update").valign(gtk::Align::Center).css_classes(["suggested-action"]).build();
            let (ctx, name) = (ctx.clone(), name.clone());
            b.connect_clicked(move |_| run::cli_and_install(&ctx, &format!("Updating {name}"), &["update", &name]));
            row.add_suffix(&b);
        }
        let edit = gtk::Button::builder().icon_name("document-edit-symbolic").tooltip_text("Edit").valign(gtk::Align::Center).css_classes(["flat"]).build();
        edit.connect_clicked({
            let (ctx, name) = (ctx.clone(), name.clone());
            move |_| source_dialog::open(&ctx, Some(&name))
        });
        row.add_suffix(&edit);
        let remove = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Stop tracking").valign(gtk::Align::Center).css_classes(["flat"]).build();
        remove.connect_clicked({
            let (ctx, name, pkg) = (ctx.clone(), name.clone(), pkg.to_string());
            move |_| confirm_remove(&ctx, &name, &pkg)
        });
        row.add_suffix(&remove);
        list.append(&row);
    }
}

fn confirm_remove(ctx: &Rc<Ctx>, name: &str, pkg: &str) {
    let body = format!("pacdeb stops updating {name}. The installed package {pkg} stays; remove it with your package manager if you want it gone.");
    let alert = adw::AlertDialog::new(Some(&format!("Stop tracking {name}?")), Some(&body));
    alert.add_response("cancel", "Cancel");
    alert.add_response("remove", "Stop tracking");
    alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    alert.set_close_response("cancel");
    let (ctx2, name) = (ctx.clone(), name.to_string());
    alert.connect_response(None, move |_, response| {
        if response != "remove" {
            return;
        }
        let ctx3 = ctx2.clone();
        run::quiet(&["remove", &name], move |done| {
            ctx3.refresh();
            ctx3.toast(done.output.lines().last().unwrap_or(if done.ok { "Removed" } else { "Could not remove" }));
        });
    });
    alert.present(Some(&ctx.window));
}
