//! The Apps page: tracked apps with their versions; Update (refresh, like apt update)
//! and Upgrade (build and install what is newer); adding, editing or removing apps.

use std::rc::Rc;

use adw::prelude::*;
use pacdeb::apps::{self, Status};
use pacdeb::paths::Paths;
use pacdeb::registry::{Config, State};
use pacdeb::sources::{Latest, release};

use crate::{Ctx, run, source_dialog};

pub fn build(ctx: &Rc<Ctx>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Tracked apps")
        .description("Update checks every source for new versions; Upgrade builds and installs them")
        .build();
    let update = gtk::Button::builder().label("Update").tooltip_text("Check every source for new versions (pacdeb update)").css_classes(["flat"]).build();
    let upgrade = gtk::Button::builder().label("Upgrade all").tooltip_text("Build and install everything newer (pacdeb upgrade)").css_classes(["flat"]).build();
    let add = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text("Add an app").css_classes(["flat"]).build();
    let buttons = gtk::Box::builder().spacing(6).build();
    buttons.append(&update);
    buttons.append(&upgrade);
    buttons.append(&add);
    group.set_header_suffix(Some(&buttons));
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    group.add(&list);
    page.add(&group);

    let fill: Rc<dyn Fn()> = {
        let (ctx, list) = (ctx.clone(), list.clone());
        Rc::new(move || fill_list(&ctx, &list))
    };
    fill();
    ctx.on_refresh({
        let fill = fill.clone();
        move || fill()
    });

    update.connect_clicked({
        let ctx = ctx.clone();
        move |_| {
            let ctx2 = ctx.clone();
            run::logged(&ctx, "Updating", &run::cli(), &["update".to_string()], false, move |log, done| {
                ctx2.refresh();
                if done.ok {
                    log.close();
                    let summary = done.output.lines().find(|l| l.contains("can be upgraded") || l.contains("up to date")).unwrap_or("Updated");
                    ctx2.toast(summary.split(" Run ").next().unwrap_or(summary));
                }
            });
        }
    });
    upgrade.connect_clicked({
        let ctx = ctx.clone();
        move |_| run::cli_and_install(&ctx, "Upgrading all apps", &["upgrade"])
    });
    add.connect_clicked({
        let ctx = ctx.clone();
        move |_| source_dialog::open(&ctx, None)
    });
    page.upcast()
}

fn fill_list(ctx: &Rc<Ctx>, list: &gtk::ListBox) {
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
        let available = s.and_then(|s| s.available.as_ref());
        if let Some(a) = available {
            parts.push(format!("checked {}", release::relative(a.checked, release::now())));
        }
        let row = adw::ActionRow::builder().title(name).subtitle(parts.join(" · ")).build();

        let found = available.map(|a| apps::status(&Latest::from_available(a), s));
        if let Some(Status::Newer { latest, .. }) = &found {
            parts.push(format!("available {latest}"));
        }
        let (label, class, newer) = match found {
            None => (String::new(), "dim-label", false),
            Some(Status::UpToDate(_)) => ("up to date".into(), "success", false),
            Some(Status::Newer { .. }) => ("update available".into(), "warning", true),
            Some(Status::Changed(true)) => ("new download".into(), "warning", true),
            Some(Status::Changed(false)) => ("unchanged".into(), "success", false),
            Some(Status::Manual) => ("manual".into(), "dim-label", false),
        };
        if !label.is_empty() {
            let l = gtk::Label::builder().label(&label).css_classes([class]).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(36).tooltip_text(&label).build();
            row.add_suffix(&l);
        }
        if newer {
            let b = gtk::Button::builder().label("Upgrade").valign(gtk::Align::Center).css_classes(["suggested-action"]).build();
            let (ctx, name) = (ctx.clone(), name.clone());
            b.connect_clicked(move |_| run::cli_and_install(&ctx, &format!("Upgrading {name}"), &["upgrade", &name]));
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
