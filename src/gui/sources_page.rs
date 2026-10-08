//! The Sources page: where the tracked apps come from. Saved apt repositories are listed
//! with their health and can be browsed, managed, and added from a vendor's apt line.

use std::rc::Rc;

use adw::prelude::*;
use pacdeb::aptrepos;
use pacdeb::paths::Paths;
use pacdeb::registry::{Config, SourceConfig};
use pacdeb::sources::release;

use crate::{Ctx, browse_dialog, repo_dialog, run};

/// A row for text from outside pacdeb (URLs, descriptions), which must not be read as markup.
pub fn plain_row(title: &str, subtitle: &str) -> adw::ActionRow {
    // Rows read their text as markup; escaping keeps <, > and & as typed.
    let esc = |s: &str| gtk::glib::markup_escape_text(s).to_string();
    adw::ActionRow::builder().title(esc(title)).subtitle(esc(subtitle)).build()
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
    let (apt_group, apt) = group(&page, "apt repositories", "Saved Debian package repositories. Apps take packages from them; browse one to see everything it offers.");
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
            let now = release::now();
            for (name, repo) in &config.apt {
                let health = aptrepos::health(name, repo, &paths, false);
                let apps = config.apps_using(name);
                let comps = if repo.is_flat() { "flat".to_string() } else { repo.components.join(" ") };
                let used = if apps.is_empty() { "no apps yet".to_string() } else { format!("used by {}", apps.join(", ")) };
                let checked = health.checked.map(|t| format!("checked {}", release::relative(t, now))).unwrap_or_else(|| "not checked yet".into());
                let row = plain_row(name, &format!("{} · {} · {comps}\n{used} · {checked}", repo.url, repo.suite));
                row.set_subtitle_lines(3);
                let serious = health.warnings.iter().any(|w| w != "never checked yet");
                if serious {
                    let icon = gtk::Image::builder().icon_name("dialog-warning-symbolic").css_classes(["warning"]).tooltip_text(health.warnings.join("\n")).build();
                    row.add_prefix(&icon);
                }
                let browse = gtk::Button::builder().label("Browse").valign(gtk::Align::Center).build();
                browse.connect_clicked({
                    let (ctx, name) = (ctx.clone(), name.clone());
                    move |_| browse_dialog::open(&ctx, &name)
                });
                let manage = gtk::Button::builder().icon_name("emblem-system-symbolic").tooltip_text("Details and settings").valign(gtk::Align::Center).css_classes(["flat"]).build();
                manage.connect_clicked({
                    let (ctx, name) = (ctx.clone(), name.clone());
                    move |_| repo_dialog::open(&ctx, &name)
                });
                row.add_suffix(&browse);
                row.add_suffix(&manage);
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
                    SourceConfig::Manual {} => manual.append(&plain_row(name, "upgraded with 'pacdeb upgrade <app> --file <deb>' or the Convert page")),
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

/// Saves a repository from a vendor's apt line (or .sources text) and key link.
fn add_repository(ctx: &Rc<Ctx>) {
    let page = adw::PreferencesPage::new();
    let what = adw::PreferencesGroup::builder()
        .title("Repository")
        .description("Paste the line from the vendor's install instructions, like:\ndeb [arch=amd64] https://example.com/apt stable main\nA whole .sources file works too.")
        .build();
    let buffer = gtk::TextBuffer::new(None);
    let text = gtk::TextView::builder()
        .buffer(&buffer)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::Char)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .css_classes(["card"])
        .height_request(90)
        .build();
    what.add(&text);
    page.add(&what);
    let details = adw::PreferencesGroup::builder().title("Signing key").description("pacdeb checks the repository's signature before saving it. A .sources file that includes the key needs no link.").build();
    let entry = |title: &str| {
        let r = adw::EntryRow::builder().title(title).build();
        details.add(&r);
        r
    };
    let key_url = entry("Key link (the .asc or .gpg file)");
    let fpr = entry("Key fingerprint (recommended, from the vendor)");
    let name = entry("Name (optional, made from the URL)");
    page.add(&details);

    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::builder().label("Add").css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    let dialog = adw::Dialog::builder().title("Add an apt repository").content_width(620).content_height(620).child(&view).build();
    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    save.connect_clicked({
        let (ctx, dialog) = (ctx.clone(), dialog.clone());
        move |_| {
            let line = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().to_string();
            if line.is_empty() {
                ctx.toast("Paste the repository's apt line first");
                return;
            }
            let mut args = vec!["apt".to_string(), "add".to_string()];
            let n = name.text().trim().to_string();
            if !n.is_empty() {
                args.push(n);
            }
            args.extend(["--line".to_string(), line]);
            for (flag, row) in [("--key-url", &key_url), ("--key-fingerprint", &fpr)] {
                let v = row.text().trim().to_string();
                if !v.is_empty() {
                    args.extend([flag.to_string(), v]);
                }
            }
            let (ctx2, form) = (ctx.clone(), dialog.clone());
            run::logged(&ctx, "Adding the repository", &run::cli(), &args, false, move |log, done| {
                ctx2.refresh();
                if done.ok {
                    log.close();
                    form.close();
                    let saved = done.output.lines().find_map(|l| l.strip_prefix("Saved apt repository ").and_then(|r| r.split('.').next()).map(String::from));
                    match saved {
                        Some(r) => {
                            ctx2.toast(&format!("Saved {r}; pick the packages to track"));
                            browse_dialog::open(&ctx2, &r);
                        }
                        None => ctx2.toast(done.output.lines().find(|l| !l.trim().is_empty()).unwrap_or("Done")),
                    }
                }
            });
        }
    });
    dialog.present(Some(&ctx.window));
}
