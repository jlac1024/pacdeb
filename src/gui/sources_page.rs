//! The Sources page: where the tracked apps come from. apt repositories can be browsed
//! for everything they offer, and new ones added and browsed before tracking anything.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use pacdeb::browse::{self, AptRepo};
use pacdeb::paths::Paths;
use pacdeb::registry::{Config, SourceConfig};
use pacdeb::sources::apt::host_arch;

use crate::browse_dialog::{self, Target};
use crate::Ctx;

/// A row for text from outside pacdeb (URLs, descriptions), which must not be read as markup.
pub fn plain_row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder().title(title).subtitle(subtitle).use_markup(false).build()
}

fn group(page: &adw::PreferencesPage, title: &str, description: &str) -> (adw::PreferencesGroup, gtk::ListBox) {
    let g = adw::PreferencesGroup::builder().title(title).description(description).build();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    g.add(&list);
    page.add(&g);
    (g, list)
}

pub fn build(ctx: &Rc<Ctx>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let (apt_group, apt) = group(&page, "apt repositories", "Debian package repositories your apps come from. Browse one to see every package it offers.");
    let add = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text("Add an apt repository").css_classes(["flat"]).build();
    apt_group.set_header_suffix(Some(&add));
    let (_, feeds) = group(&page, "Download links and feeds", "Apps downloaded from a fixed link, or from a link a version feed names");
    let (_, github) = group(&page, "GitHub releases", "Apps taken from release downloads on GitHub");
    let (_, manual) = group(&page, "Added by hand", "Apps updated with .deb files you give pacdeb");

    let fill = {
        let ctx = ctx.clone();
        move || {
            for l in [&apt, &feeds, &github, &manual] {
                l.remove_all();
            }
            let Ok(paths) = Paths::from_env() else {
                return;
            };
            let config = Config::load(&paths.config).unwrap_or_default();
            for used in browse::used_repos(&config, &paths) {
                let r = &used.repo;
                let subtitle = format!("{} · {} · {} · used by {}", r.suite, r.component, r.arch, used.apps.join(", "));
                let row = plain_row(&r.repo, &subtitle);
                let browse = gtk::Button::builder().label("Browse").valign(gtk::Align::Center).build();
                match used.key.clone() {
                    Some(key) => {
                        let (ctx, repo, tracked) = (ctx.clone(), r.clone(), used.packages.clone());
                        browse.connect_clicked(move |_| browse_dialog::open(&ctx, Target { repo: repo.clone(), key: key.clone(), fingerprint: None, tracked: tracked.clone() }));
                    }
                    None => {
                        browse.set_sensitive(false);
                        browse.set_tooltip_text(Some("No signing key; set one on the app first"));
                    }
                }
                row.add_suffix(&browse);
                apt.append(&row);
            }
            for (name, app) in &config.apps {
                match &app.source {
                    SourceConfig::Direct { url, feed, .. } => {
                        let link = feed.as_ref().map(|f| format!("feed {f}")).or_else(|| url.clone()).unwrap_or_default();
                        feeds.append(&plain_row(name, &link));
                    }
                    SourceConfig::Github { repo, asset, prerelease } => {
                        let pre = if *prerelease { " · with prereleases" } else { "" };
                        github.append(&plain_row(repo, &format!("{asset} · used by {name}{pre}")));
                    }
                    SourceConfig::Manual {} => manual.append(&plain_row(name, "updated with 'pacdeb update <app> --file <deb>' or the Convert page")),
                    SourceConfig::Apt { .. } => {}
                }
            }
            for l in [&apt, &feeds, &github, &manual] {
                if l.first_child().is_none() {
                    l.append(&adw::ActionRow::builder().title("None").css_classes(["dim-label"]).build());
                }
            }
        }
    };
    fill();
    ctx.on_refresh(fill);
    add.connect_clicked({
        let ctx = ctx.clone();
        move |_| add_repository(&ctx)
    });
    page.upcast()
}

/// Asks for a repository and its key, checks them, then opens the package browser.
fn add_repository(ctx: &Rc<Ctx>) {
    let page = adw::PreferencesPage::new();
    let g = adw::PreferencesGroup::builder()
        .description("pacdeb checks the repository's signature before showing its packages. The vendor's install instructions name these values (the 'deb' line and the key link).")
        .build();
    let entry = |title: &str, text: &str| {
        let r = adw::EntryRow::builder().title(title).text(text).build();
        g.add(&r);
        r
    };
    let repo = entry("Repository URL", "");
    let suite = entry("Suite", "stable");
    let component = entry("Component", "main");
    let key_url = entry("Signing key URL", "");
    let fpr = entry("Key fingerprint (recommended)", "");
    page.add(&g);

    let cancel = gtk::Button::with_label("Cancel");
    let next = gtk::Button::builder().label("Browse packages").css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
    header.pack_start(&cancel);
    header.pack_end(&next);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    let dialog = adw::Dialog::builder().title("Add an apt repository").content_width(560).child(&view).build();
    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    next.connect_clicked({
        let (ctx, dialog) = (ctx.clone(), dialog.clone());
        move |button| {
            let text = |r: &adw::EntryRow| r.text().trim().to_string();
            let (r, s, c, k, f) = (text(&repo), text(&suite), text(&component), text(&key_url), text(&fpr));
            if r.is_empty() || s.is_empty() || c.is_empty() || k.is_empty() {
                ctx.toast("The repository URL, suite, component and key URL are all needed");
                return;
            }
            button.set_sensitive(false);
            let (ctx, dialog, button) = (ctx.clone(), dialog.clone(), button.clone());
            glib::spawn_future_local(async move {
                let fetched = gio::spawn_blocking({
                    let (k, f) = (k.clone(), f.clone());
                    move || {
                        let paths = Paths::from_env().map_err(|e| e.to_string())?;
                        browse::fetch_key(&k, Some(&f), &paths).map_err(|e| e.to_string())
                    }
                })
                .await;
                button.set_sensitive(true);
                match fetched {
                    Ok(Ok((key, fprs))) => {
                        dialog.close();
                        let pinned = (!f.is_empty()).then(|| fprs.join(" "));
                        let repo = AptRepo { repo: r.trim_end_matches('/').to_string(), suite: s, component: c, arch: host_arch().to_string() };
                        if pinned.is_none() {
                            ctx.toast(&format!("Key fingerprint {}; compare it with the one the vendor publishes", fprs.join(", ")));
                        }
                        browse_dialog::open(&ctx, Target { repo, key, fingerprint: pinned, tracked: Vec::new() });
                    }
                    Ok(Err(e)) => ctx.toast(&e),
                    Err(_) => ctx.toast("Fetching the key stopped unexpectedly"),
                }
            });
        }
    });
    dialog.present(Some(&ctx.window));
}
