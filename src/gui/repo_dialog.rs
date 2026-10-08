// SPDX-License-Identifier: AGPL-3.0-or-later
//! One saved apt repository: its health (checked live), its settings, its signing key,
//! the apps using it, and removing it. Changes run `pacdeb apt ...`, which checks them
//! against the live repository before saving.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use pacdeb::aptrepos::{self, Health};
use pacdeb::paths::Paths;
use pacdeb::registry::{AptRepoConfig, Config};
use pacdeb::sources::release;

use crate::sources_page::plain_row;
use crate::{Ctx, browse_dialog, run};

pub fn open(ctx: &Rc<Ctx>, name: &str) {
    let Ok(paths) = Paths::from_env() else {
        return;
    };
    let config = Config::load(&paths.config).unwrap_or_default();
    let Some(repo) = config.apt.get(name).cloned() else {
        ctx.toast("That repository is no longer saved");
        ctx.refresh();
        return;
    };
    let apps: Vec<String> = config.apps_using(name).into_iter().map(String::from).collect();
    let page = adw::PreferencesPage::new();

    // Health, filled in once the live check is done.
    let status = adw::PreferencesGroup::builder().title("Status").description("Checking the repository...").build();
    let status_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    status.add(&status_list);
    page.add(&status);

    // Settings.
    let settings = adw::PreferencesGroup::builder().title("Repository").description("Changes apply to every app using it, and are saved only if the changed repository works.").build();
    let entry = |title: &str, text: &str| {
        let r = adw::EntryRow::builder().title(title).text(text).build();
        settings.add(&r);
        r
    };
    let url = entry("URL", &repo.url);
    let suite = entry("Suite", &repo.suite);
    let comps = entry("Components (space separated)", &repo.components.join(" "));
    let arch = entry("Architecture (empty: this machine's)", repo.arch.as_deref().unwrap_or(""));
    let save = gtk::Button::builder().label("Save changes").halign(gtk::Align::End).margin_top(8).css_classes(["suggested-action"]).build();
    settings.add(&save);
    page.add(&settings);

    // Signing key.
    let key = adw::PreferencesGroup::builder().title("Signing key").build();
    let key_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    key.add(&key_list);
    let key_url = adw::EntryRow::builder().title("Key link").text(repo.key_url.as_deref().unwrap_or("")).build();
    let key_fpr = adw::EntryRow::builder().title("Pinned fingerprint (empty: not pinned)").text(repo.key_fingerprint.as_deref().unwrap_or("")).build();
    let key_rows = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).margin_top(12).build();
    key_rows.append(&key_url);
    key_rows.append(&key_fpr);
    key.add(&key_rows);
    let fetch_key = gtk::Button::builder().label("Fetch the key again").halign(gtk::Align::End).margin_top(8).build();
    key.add(&fetch_key);
    page.add(&key);

    // Apps.
    let apps_group = adw::PreferencesGroup::builder().title("Apps").build();
    let browse = gtk::Button::builder().label("Browse packages").css_classes(["flat"]).build();
    apps_group.set_header_suffix(Some(&browse));
    let apps_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    for a in &apps {
        apps_list.append(&plain_row(a, ""));
    }
    if apps.is_empty() {
        apps_list.append(&adw::ActionRow::builder().title("No apps use this repository yet").css_classes(["dim-label"]).build());
    }
    apps_group.add(&apps_list);
    page.add(&apps_group);

    let danger = adw::PreferencesGroup::new();
    let remove = gtk::Button::builder().label("Remove repository").halign(gtk::Align::Center).css_classes(["destructive-action", "pill"]).build();
    danger.add(&remove);
    page.add(&danger);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&page));
    let dialog = adw::Dialog::builder().title(name).content_width(640).content_height(720).child(&view).build();
    dialog.present(Some(&ctx.window));

    // Live check.
    {
        let (name, repo) = (name.to_string(), repo.clone());
        let (status, status_list, key_list) = (status.clone(), status_list.clone(), key_list.clone());
        let repo_for_check = repo.clone();
        glib::spawn_future_local(async move {
            let checked = gio::spawn_blocking(move || {
                let paths = Paths::from_env().ok()?;
                Some(aptrepos::health(&name, &repo_for_check, &paths, true))
            })
            .await;
            if let Ok(Some(h)) = checked {
                show_health(&h, &repo, &status, &status_list, &key_list);
            } else {
                status.set_description(Some("Could not check the repository"));
            }
        });
    }

    save.connect_clicked({
        let (ctx, name, repo, dialog) = (ctx.clone(), name.to_string(), repo.clone(), dialog.clone());
        move |_| {
            let mut args = vec!["apt".to_string(), "edit".to_string(), name.clone()];
            let changed = |flag: &str, now: String, before: String, args: &mut Vec<String>| {
                if now != before {
                    args.extend([flag.to_string(), now]);
                }
            };
            changed("--url", url.text().trim().to_string(), repo.url.clone(), &mut args);
            changed("--suite", suite.text().trim().to_string(), repo.suite.clone(), &mut args);
            changed("--components", comps.text().split_whitespace().collect::<Vec<_>>().join(","), repo.components.join(","), &mut args);
            changed("--arch", arch.text().trim().to_string(), repo.arch.clone().unwrap_or_default(), &mut args);
            if args.len() == 3 {
                ctx.toast("Nothing changed");
                return;
            }
            let (ctx2, dialog, name) = (ctx.clone(), dialog.clone(), name.clone());
            run::logged(&ctx, "Changing the repository", &run::cli(), &args, false, move |log, done| {
                ctx2.refresh();
                if done.ok {
                    log.close();
                    dialog.close();
                    ctx2.toast(&format!("Updated {name}"));
                }
            });
        }
    });

    fetch_key.connect_clicked({
        let (ctx, name, repo, dialog) = (ctx.clone(), name.to_string(), repo.clone(), dialog.clone());
        move |_| {
            let mut args = vec!["apt".to_string(), "key".to_string(), name.clone()];
            let u = key_url.text().trim().to_string();
            if !u.is_empty() && Some(u.as_str()) != repo.key_url.as_deref() {
                args.extend(["--key-url".to_string(), u]);
            }
            let f = key_fpr.text().trim().to_string();
            if f != repo.key_fingerprint.clone().unwrap_or_default() {
                args.extend(["--key-fingerprint".to_string(), f]);
            }
            let (ctx2, dialog) = (ctx.clone(), dialog.clone());
            run::logged(&ctx, "Signing key", &run::cli(), &args, false, move |_, done| {
                ctx2.refresh();
                if done.ok {
                    dialog.close();
                }
            });
        }
    });

    browse.connect_clicked({
        let (ctx, name) = (ctx.clone(), name.to_string());
        move |_| browse_dialog::open(&ctx, &name)
    });

    remove.connect_clicked({
        let (ctx, name, dialog) = (ctx.clone(), name.to_string(), dialog.clone());
        move |_| {
            let body = if apps.is_empty() {
                "Its signing key is deleted too.".to_string()
            } else {
                format!("pacdeb also stops tracking {}. Installed packages stay installed.", apps.join(", "))
            };
            let alert = adw::AlertDialog::new(Some(&format!("Remove {name}?")), Some(&body));
            alert.add_response("cancel", "Cancel");
            alert.add_response("remove", "Remove");
            alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            alert.set_close_response("cancel");
            let (ctx2, name, dialog) = (ctx.clone(), name.clone(), dialog.clone());
            let with_apps = !apps.is_empty();
            alert.connect_response(None, move |_, response| {
                if response != "remove" {
                    return;
                }
                let mut args = vec!["apt", "remove", name.as_str()];
                if with_apps {
                    args.push("--with-apps");
                }
                let (ctx3, dialog) = (ctx2.clone(), dialog.clone());
                run::quiet(&args, move |done| {
                    ctx3.refresh();
                    if done.ok {
                        dialog.close();
                    }
                    ctx3.toast(done.output.lines().last().unwrap_or(if done.ok { "Removed" } else { "Could not remove" }));
                });
            });
            alert.present(Some(&ctx.window));
        }
    });
}

fn show_health(h: &Health, repo: &AptRepoConfig, status: &adw::PreferencesGroup, list: &gtk::ListBox, keys: &gtk::ListBox) {
    let now = release::now();
    list.remove_all();
    status.set_description(None);
    if let Some(e) = &h.error {
        let row = plain_row("Could not read the repository", e);
        row.set_subtitle_lines(6);
        row.add_prefix(&gtk::Image::builder().icon_name("dialog-error-symbolic").css_classes(["error"]).build());
        list.append(&row);
    }
    for w in &h.warnings {
        let row = plain_row(w, "");
        row.add_prefix(&gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).build());
        list.append(&row);
    }
    let date = |v: &Option<String>| v.as_deref().map(|d| match release::parse_date(d) {
        Some(t) => format!("{d} ({})", release::relative(t, now)),
        None => d.to_string(),
    });
    if let Some(info) = &h.info {
        let who: Vec<&str> = [info.origin.as_deref(), info.label.as_deref()].into_iter().flatten().collect();
        for (title, value) in [
            ("Published by", (!who.is_empty()).then(|| who.join(" / "))),
            ("Updated", date(&info.date)),
            ("Valid until", date(&info.valid_until)),
            ("Offers", (!info.components.is_empty()).then(|| format!("{} for {}", info.components.join(" "), info.architectures.join(" ")))),
        ] {
            if let Some(v) = value {
                list.append(&plain_row(title, &v));
            }
        }
    }
    list.append(&plain_row("Checked", &h.checked.map(|t| release::relative(t, now)).unwrap_or_else(|| "never".into())));

    keys.remove_all();
    for k in &h.keys {
        let expiry = match k.expires {
            Some(t) => format!("expires {}", release::relative(t, now)),
            None => "does not expire".into(),
        };
        let pinned = repo.key_fingerprint.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(&k.fingerprint));
        let row = plain_row(&k.fingerprint, &format!("{}{expiry}{}", k.uid.as_deref().map(|u| format!("{u} · ")).unwrap_or_default(), if pinned { " · pinned" } else { "" }));
        row.set_subtitle_lines(2);
        keys.append(&row);
    }
    if h.keys.is_empty() {
        keys.append(&adw::ActionRow::builder().title("No signing key").css_classes(["dim-label"]).build());
    }
}
