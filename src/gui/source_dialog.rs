// SPDX-License-Identifier: AGPL-3.0-or-later
//! Adding an app, or editing one. The form turns into `pacdeb add` or `pacdeb set`
//! arguments, so the command line tool checks and saves everything (including
//! fetching and checking signing keys) the same way it does in a terminal.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use pacdeb::paths::Paths;
use pacdeb::registry::{App, Config, SourceConfig};

use crate::{Ctx, run};

const KINDS: &[(&str, &str)] = &[("apt", "Saved apt repository"), ("direct", "Direct download"), ("github", "GitHub releases"), ("manual", "Manual (.deb files you add)")];

struct Form {
    name: adw::EntryRow,
    kind: adw::ComboRow,
    kinds: Vec<&'static str>,
    /// Text fields: flag, row, and which source kind they belong to ("" for any).
    fields: Vec<(&'static str, &'static str, adw::EntryRow)>,
    prerelease: adw::SwitchRow,
    /// The saved apt repositories, for apt sources.
    apt_repo: adw::ComboRow,
    apt_names: Vec<String>,
    before_apt: Option<String>,
    groups: Vec<(&'static str, adw::PreferencesGroup)>,
    /// What the fields held when the dialog opened, for an edit.
    before: BTreeMap<&'static str, String>,
    before_kind: Option<&'static str>,
    before_prerelease: bool,
}

fn entry(title: &str) -> adw::EntryRow {
    adw::EntryRow::builder().title(title).build()
}

impl Form {
    fn kind(&self) -> &'static str {
        self.kinds.get(self.kind.selected() as usize).copied().unwrap_or("manual")
    }

    fn show_groups(&self) {
        let kind = self.kind();
        for (k, g) in &self.groups {
            g.set_visible(*k == kind);
        }
    }

    fn selected_apt(&self) -> Option<String> {
        self.apt_names.get(self.apt_repo.selected() as usize).cloned()
    }

    /// The command line arguments that save the form.
    fn args(&self, editing: Option<&str>) -> Result<Vec<String>, String> {
        let kind = self.kind();
        let mut args: Vec<String> = Vec::new();
        let push = |args: &mut Vec<String>, flag: &str, v: String| {
            args.push(flag.to_string());
            args.push(v);
        };
        match editing {
            None => {
                let name = self.name.text().trim().to_string();
                if name.is_empty() {
                    return Err("Give the app a name".into());
                }
                args.extend(["add".into(), name, "--source".into(), kind.into()]);
                for (flag, k, row) in &self.fields {
                    let v = row.text().trim().to_string();
                    if (*k == kind || k.is_empty()) && !v.is_empty() {
                        push(&mut args, flag, v);
                    }
                }
                if kind == "github" && self.prerelease.is_active() {
                    args.push("--prerelease".into());
                }
                if kind == "apt" {
                    let repo = self.selected_apt().ok_or("Add an apt repository on the Sources page first")?;
                    push(&mut args, "--apt", repo);
                }
            }
            Some(name) => {
                args.extend(["set".into(), name.to_string()]);
                let replacing = self.before_kind != Some(kind);
                if replacing {
                    push(&mut args, "--source", kind.into());
                }
                for (flag, k, row) in &self.fields {
                    if !(*k == kind || k.is_empty()) {
                        continue;
                    }
                    let v = row.text().trim().to_string();
                    let changed = self.before.get(flag).map(String::as_str).unwrap_or("") != v;
                    let is_key = flag.starts_with("--key");
                    if (replacing && !v.is_empty()) || (!replacing && changed && !(is_key && v.is_empty())) {
                        if v.is_empty() && ["--repo", "--suite", "--asset"].contains(flag) {
                            return Err(format!("{} cannot be empty", row.title()));
                        }
                        push(&mut args, flag, v);
                    }
                }
                if kind == "apt" {
                    let repo = self.selected_apt().ok_or("Add an apt repository on the Sources page first")?;
                    if replacing || self.before_apt.as_deref() != Some(repo.as_str()) {
                        push(&mut args, "--apt", repo);
                    }
                }
                if kind == "github" && (replacing || self.prerelease.is_active() != self.before_prerelease) {
                    args.push(if self.prerelease.is_active() { "--prerelease" } else { "--no-prerelease" }.into());
                }
                if args.len() == 2 {
                    return Err("Nothing changed".into());
                }
            }
        }
        Ok(args)
    }
}

fn build_form(editing: Option<(&str, &App, &Config)>) -> (Form, adw::PreferencesPage) {
    let kinds: Vec<&'static str> = KINDS.iter().map(|(k, _)| *k).collect();
    let labels: Vec<&str> = kinds.iter().map(|k| KINDS.iter().find(|(kk, _)| kk == k).map(|(_, l)| *l).unwrap_or(k)).collect();
    let page = adw::PreferencesPage::new();

    let general = adw::PreferencesGroup::new();
    let name = entry("Name");
    let kind = adw::ComboRow::builder().title("Source").model(&gtk::StringList::new(&labels)).build();
    general.add(&name);
    general.add(&kind);

    let mut fields: Vec<(&'static str, &'static str, adw::EntryRow)> = Vec::new();
    let mut groups = Vec::new();
    let mut group = |kind_key: &'static str, title: &str, rows: &[(&'static str, &str)], fields: &mut Vec<(&'static str, &'static str, adw::EntryRow)>| {
        let g = adw::PreferencesGroup::builder().title(title).build();
        for (flag, label) in rows {
            let r = entry(label);
            g.add(&r);
            fields.push((flag, kind_key, r));
        }
        page.add(&g);
        groups.push((kind_key, g.clone()));
        g
    };
    page.add(&general);
    let options = adw::PreferencesGroup::builder().title("Options").description("Leave empty for the defaults").build();
    for (flag, label) in [("--channel", "Channel, e.g. Stable"), ("--pkgname", "Package name (default: the deb's, -deb added on a clash)")] {
        let r = entry(label);
        options.add(&r);
        fields.push((flag, "", r));
    }
    group(
        "direct",
        "Direct download",
        &[
            ("--url", "Download URL (may contain {version})"),
            ("--feed", "Feed URL that names the newest version"),
            ("--version-json", "Version in a JSON feed, e.g. Release.Version"),
            ("--version-regex", "Version regex for a text feed"),
            ("--version-pattern", "Version pattern, e.g. app_{version}_amd64.deb"),
            ("--url-json", "Download URL in a JSON feed"),
            ("--checksum-json", "Checksum in a JSON feed"),
        ],
        &mut fields,
    );
    let apt_names: Vec<String> = editing.map(|(_, _, c)| c.apt.keys().cloned().collect()).unwrap_or_else(saved_repositories);
    let apt_labels: Vec<&str> = apt_names.iter().map(String::as_str).collect();
    let apt_repo = adw::ComboRow::builder().title("Repository").model(&gtk::StringList::new(&apt_labels)).build();
    let apt_group = group("apt", "Saved apt repository", &[("--package", "Package (default: the app name)")], &mut fields);
    apt_group.set_description(Some(if apt_names.is_empty() { "No apt repositories saved yet; add one on the Sources page." } else { "Repositories are added and managed on the Sources page." }));
    apt_group.add(&apt_repo);
    let gh = group("github", "GitHub releases", &[("--repo", "Repository, owner/name"), ("--asset", "Asset pattern, e.g. *_amd64.deb")], &mut fields);
    let prerelease = adw::SwitchRow::builder().title("Include prereleases").build();
    gh.add(&prerelease);
    page.add(&options);

    let mut form = Form { name, kind, kinds, fields, prerelease, apt_repo, apt_names, before_apt: None, groups, before: BTreeMap::new(), before_kind: None, before_prerelease: false };
    if let Some((app_name, app, config)) = editing {
        form.name.set_text(app_name);
        form.name.set_editable(false);
        let mut before: BTreeMap<&'static str, String> = BTreeMap::new();
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();
        before.insert("--channel", opt(&app.channel));
        before.insert("--pkgname", opt(&app.pkgname));
        let kind_key = app.source.kind();
        match &app.source {
            SourceConfig::Direct { url, feed, version_json, version_pattern, version_regex, url_json, checksum_json, .. } => {
                for (f, v) in [("--url", url), ("--feed", feed), ("--version-json", version_json), ("--version-regex", version_regex), ("--version-pattern", version_pattern), ("--url-json", url_json), ("--checksum-json", checksum_json)] {
                    before.insert(f, opt(v));
                }
            }
            SourceConfig::Apt { repository, package } => {
                before.insert("--package", opt(package));
                if let Some(i) = form.apt_names.iter().position(|n| n == repository) {
                    form.apt_repo.set_selected(i as u32);
                }
                form.before_apt = Some(repository.clone());
            }
            SourceConfig::Github { repo, asset, prerelease } => {
                before.insert("--repo", repo.clone());
                before.insert("--asset", asset.clone());
                form.before_prerelease = *prerelease;
                form.prerelease.set_active(*prerelease);
            }
            SourceConfig::Manual {} => {}
        }
        for (flag, k, row) in &form.fields {
            if k.is_empty() || *k == kind_key {
                row.set_text(before.get(flag).map(String::as_str).unwrap_or(""));
            }
        }
        let shown = config.channel(app).filter(|_| app.channel.is_none() && app.source.uses_channel());
        if let Some(c) = shown {
            form.fields[0].2.set_title(&format!("Channel (now {c} from the global setting or default)"));
        }
        form.before = before;
        form.before_kind = form.kinds.iter().copied().find(|k| *k == kind_key);
        if let Some(i) = form.kinds.iter().position(|k| *k == kind_key) {
            form.kind.set_selected(i as u32);
        }
    }
    (form, page)
}

fn saved_repositories() -> Vec<String> {
    Paths::from_env().and_then(|p| Config::load(&p.config)).map(|c| c.apt.keys().cloned().collect()).unwrap_or_default()
}

/// Opens the dialog for a new app (`None`) or for editing a tracked one.
pub fn open(ctx: &Rc<Ctx>, editing: Option<&str>) {
    let config = match Paths::from_env().and_then(|p| Config::load(&p.config)) {
        Ok(c) => c,
        Err(e) => {
            ctx.toast(&e.to_string());
            return;
        }
    };
    let app = editing.and_then(|n| config.apps.get(n));
    if editing.is_some() && app.is_none() {
        ctx.toast("That app is no longer tracked");
        ctx.refresh();
        return;
    }
    let (form, page) = build_form(editing.zip(app).map(|(n, a)| (n, a, &config)));
    let form = Rc::new(form);
    form.show_groups();
    form.kind.connect_selected_notify({
        let form = form.clone();
        move |_| form.show_groups()
    });

    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::builder().label(if editing.is_some() { "Save" } else { "Add" }).css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    let title = match editing {
        Some(n) => format!("Edit {n}"),
        None => "Add an app".to_string(),
    };
    let dialog = adw::Dialog::builder().title(&title).content_width(560).content_height(640).child(&view).build();
    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    let editing = editing.map(String::from);
    save.connect_clicked({
        let (ctx, dialog, form) = (ctx.clone(), dialog.clone(), form.clone());
        move |_| {
            let args = match form.args(editing.as_deref()) {
                Ok(a) => a,
                Err(e) => {
                    ctx.toast(&e);
                    return;
                }
            };
            let (ctx2, form_dialog) = (ctx.clone(), dialog.clone());
            run::logged(&ctx, "Saving", &run::cli(), &args, false, move |log, done| {
                ctx2.refresh();
                if done.ok {
                    log.close();
                    form_dialog.close();
                    ctx2.toast(done.output.lines().find(|l| !l.trim().is_empty()).unwrap_or("Saved"));
                }
            });
        }
    });
    dialog.present(Some(&ctx.window));
}
