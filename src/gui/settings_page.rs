//! The Settings page: the global channel, scheduled checks, and the local repository.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use pacdeb::paths::Paths;
use pacdeb::registry::Config;

use crate::{Ctx, run};

pub fn build(ctx: &Rc<Ctx>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();

    let channels = adw::PreferencesGroup::builder()
        .title("Channel")
        .description("Used by apps that publish several channels (such as Proton Mail's Stable, EarlyAccess and Alpha) and have none of their own")
        .build();
    let channel = adw::EntryRow::builder().title("Global channel").show_apply_button(true).build();
    channels.add(&channel);
    page.add(&channels);

    let schedule = adw::PreferencesGroup::builder().title("Updates").build();
    let timer = adw::SwitchRow::builder().title("Check for updates automatically").subtitle("5 minutes after login, then every 6 hours").build();
    schedule.add(&timer);
    page.add(&schedule);

    let repo_group = adw::PreferencesGroup::builder().title("Local repository").build();
    let repo = adw::ActionRow::builder().title("Repository").subtitle_selectable(true).build();
    let repo_pkgs = adw::ActionRow::builder().title("Packages").build();
    repo_group.add(&repo);
    repo_group.add(&repo_pkgs);
    page.add(&repo_group);

    let folders = adw::PreferencesGroup::builder().title("Folders").build();
    let rows: Vec<adw::ActionRow> = ["Settings", "Build records", "Downloads and builds"]
        .iter()
        .map(|t| {
            let r = adw::ActionRow::builder().title(*t).subtitle_selectable(true).build();
            folders.add(&r);
            r
        })
        .collect();
    page.add(&folders);

    // Set while the page itself changes the switch, so that does not run the timer command.
    let loading = Rc::new(Cell::new(false));
    let load = {
        let (channel, timer, repo, repo_pkgs, loading) = (channel.clone(), timer.clone(), repo.clone(), repo_pkgs.clone(), loading.clone());
        move || {
            let Ok(paths) = Paths::from_env() else {
                return;
            };
            let config = Config::load(&paths.config).unwrap_or_default();
            channel.set_text(config.settings.channel.as_deref().unwrap_or(""));
            loading.set(true);
            timer.set_active(pacdeb::timer::is_enabled());
            loading.set(false);
            match &config.settings.repo {
                Some(r) => {
                    repo.set_subtitle(&glib::markup_escape_text(&format!("[{}] in {} · signing key {}", r.name, r.dir, r.key)));
                    let count = std::fs::read_dir(&r.dir).map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".pkg.tar.zst")).count()).unwrap_or(0);
                    repo_pkgs.set_subtitle(&format!("{count} published; your system updates install new versions from here"));
                    repo_pkgs.set_visible(true);
                }
                None => {
                    repo.set_subtitle("Not set up. Run setup.sh in the pacdeb folder so system updates also update pacdeb apps.");
                    repo_pkgs.set_visible(false);
                }
            }
            for (row, dir) in rows.iter().zip([&paths.config, &paths.state, &paths.cache]) {
                row.set_subtitle(&glib::markup_escape_text(&dir.display().to_string()));
            }
        }
    };
    load();
    ctx.on_refresh(load);

    channel.connect_apply({
        let ctx = ctx.clone();
        move |row| {
            let value = row.text().trim().to_string();
            let ctx2 = ctx.clone();
            run::quiet(&["set", "--channel", &value], move |done| {
                ctx2.toast(done.output.lines().next().unwrap_or(if done.ok { "Saved" } else { "Could not save" }));
                ctx2.refresh();
            });
        }
    });
    timer.connect_active_notify({
        let ctx = ctx.clone();
        move |row| {
            if loading.get() {
                return;
            }
            let action = if row.is_active() { "enable" } else { "disable" };
            let ctx2 = ctx.clone();
            run::quiet(&["timer", action], move |done| {
                let msg = if done.ok { done.output.lines().next().unwrap_or("Done").to_string() } else { format!("Could not change scheduled checks: {}", done.output.trim()) };
                ctx2.toast(&msg);
                ctx2.refresh();
            });
        }
    });
    page.upcast()
}
