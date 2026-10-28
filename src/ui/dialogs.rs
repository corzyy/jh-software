//! Preferences, software repositories, local file installation and about dialogs.

use crate::backend::{flatpak, packagekit, rpm};
use crate::config::{self, APP_ID, APP_NAME, VERSION};
use crate::model::SourceKind;
use crate::state::{background, Change, State};
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

const ORDERS: [[SourceKind; 3]; 6] = [
    [SourceKind::Rpm, SourceKind::Terra, SourceKind::Flatpak],
    [SourceKind::Terra, SourceKind::Rpm, SourceKind::Flatpak],
    [SourceKind::Rpm, SourceKind::Flatpak, SourceKind::Terra],
    [SourceKind::Terra, SourceKind::Flatpak, SourceKind::Rpm],
    [SourceKind::Flatpak, SourceKind::Rpm, SourceKind::Terra],
    [SourceKind::Flatpak, SourceKind::Terra, SourceKind::Rpm],
];

fn switch_row(title: &str, subtitle: &str, active: bool, on: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let r = adw::SwitchRow::builder().title(title).subtitle(subtitle).active(active).build();
    r.connect_active_notify(move |r| on(r.is_active()));
    r
}

pub fn preferences(parent: &impl IsA<gtk::Widget>, state: &Rc<State>) {
    let dialog = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::builder().title("General").icon_name("preferences-system-symbolic").build();
    let cfg = state.config.borrow().clone();

    // Sources
    let g = adw::PreferencesGroup::builder()
        .title("Installation Sources")
        .description("When an app is available from several sources, the first one in this order is used by default. You can always pick another source on the app page.")
        .build();
    let labels: Vec<String> = ORDERS
        .iter()
        .map(|o| o.iter().map(|k| k.label()).collect::<Vec<_>>().join(" → "))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let combo = adw::ComboRow::builder()
        .title("Source Priority")
        .model(&gtk::StringList::new(&label_refs))
        .build();
    let cur = ORDERS.iter().position(|o| o.as_slice() == cfg.source_priority.as_slice()).unwrap_or(0);
    combo.set_selected(cur as u32);
    let st = state.clone();
    combo.connect_selected_notify(move |c| {
        if let Some(o) = ORDERS.get(c.selected() as usize) {
            let mut cfg = st.config.borrow_mut();
            cfg.source_priority = o.to_vec();
            cfg.save();
            drop(cfg);
            st.emit(Change::Installed);
        }
    });
    g.add(&combo);
    let st = state.clone();
    g.add(&switch_row(
        "Install Flatpaks for This User Only",
        "Use the per-user Flatpak installation instead of the system-wide one",
        cfg.flatpak_user,
        move |v| {
            let mut c = st.config.borrow_mut();
            c.flatpak_user = v;
            c.save();
        },
    ));
    page.add(&g);

    // Updates
    let g = adw::PreferencesGroup::builder().title("Updates").build();
    let st = state.clone();
    g.add(&switch_row("Check for Updates Automatically", "Look for new updates a few times a day", cfg.auto_check_updates, move |v| {
        let mut c = st.config.borrow_mut();
        c.auto_check_updates = v;
        c.save();
    }));
    let st = state.clone();
    g.add(&switch_row(
        "Update Flatpaks Automatically",
        "Flatpak apps are sandboxed and can be updated safely in the background",
        cfg.auto_update_flatpaks,
        move |v| {
            let mut c = st.config.borrow_mut();
            c.auto_update_flatpaks = v;
            c.save();
        },
    ));
    let st = state.clone();
    g.add(&switch_row(
        "Run in Background",
        "Start on login and keep running to notify about updates",
        cfg.run_in_background,
        move |v| {
            let mut c = st.config.borrow_mut();
            c.run_in_background = v;
            c.save();
            config::set_autostart(v);
            if let Some(app) = gio::Application::default() {
                app.activate_action("background-changed", None);
            }
        },
    ));
    page.add(&g);

    // Browsing
    let g = adw::PreferencesGroup::builder().title("Browsing").build();
    let st = state.clone();
    g.add(&switch_row("Show Proprietary Software", "Include non-free apps on the Discover page", cfg.show_nonfree, move |v| {
        let mut c = st.config.borrow_mut();
        c.show_nonfree = v;
        c.save();
        drop(c);
        st.emit(Change::Catalog);
    }));
    let st = state.clone();
    g.add(&switch_row(
        "Show Plain Packages in Search",
        "Include RPM packages without app metadata (for example most of Terra)",
        cfg.show_packages,
        move |v| {
            let mut c = st.config.borrow_mut();
            c.show_packages = v;
            c.save();
        },
    ));
    page.add(&g);

    // Maintenance
    let g = adw::PreferencesGroup::builder().title("Maintenance").build();
    let row = adw::ActionRow::builder()
        .title("Rebuild Software Catalog")
        .subtitle("Re-read all AppStream metadata and the Terra package list")
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("view-refresh-symbolic"));
    let st = state.clone();
    row.connect_activated(move |r| {
        r.set_sensitive(false);
        let r = r.clone();
        let st = st.clone();
        let _ = std::fs::remove_file(config::cache_dir().join("catalog.bin"));
        glib::spawn_future_local(async move {
            if let Some(c) = background(crate::catalog::build).await {
                *st.catalog.borrow_mut() = std::sync::Arc::new(c);
                st.emit(Change::Catalog);
                st.toast("Software catalog rebuilt");
            }
            r.set_sensitive(true);
        });
    });
    g.add(&row);
    page.add(&g);

    dialog.add(&page);
    dialog.present(Some(parent));
}

pub fn repositories(parent: &impl IsA<gtk::Widget>, state: &Rc<State>) {
    let dialog = adw::PreferencesDialog::builder().title("Software Repositories").build();
    let page = adw::PreferencesPage::new();

    let rpm_group = adw::PreferencesGroup::builder()
        .title("RPM Repositories")
        .description("Package repositories managed by dnf. Terra is a community repository with many apps not in Fedora.")
        .build();
    let loading = adw::ActionRow::builder().title("Loading…").build();
    rpm_group.add(&loading);
    page.add(&rpm_group);

    let fp_group = adw::PreferencesGroup::builder()
        .title("Flatpak Remotes")
        .description("Sources of sandboxed Flatpak apps")
        .build();
    let add = gtk::Button::builder().label("Add…").css_classes(["flat"]).build();
    let st = state.clone();
    add.connect_clicked(move |b| choose_file(b.upcast_ref(), &st, &["*.flatpakrepo"], "Flatpak Repository"));
    fp_group.set_header_suffix(Some(&add));
    page.add(&fp_group);
    dialog.add(&page);

    let st = state.clone();
    let rpm_g = rpm_group.downgrade();
    let fp_g = fp_group.downgrade();
    glib::spawn_future_local(async move {
        let res = background(|| (packagekit::repo_list(), flatpak::list_remotes())).await;
        let (Some(rpm_group), Some(fp_group)) = (rpm_g.upgrade(), fp_g.upgrade()) else { return };
        rpm_group.remove(&loading);
        let Some((repos, remotes)) = res else { return };
        match repos {
            Ok(repos) => {
                // Terra first, then enabled, then the rest.
                let mut repos = repos;
                repos.sort_by_key(|r| (!crate::backend::terra::is_terra_repo(&r.id), !r.enabled, r.id.clone()));
                for r in repos {
                    let row = adw::SwitchRow::builder()
                        .title(glib::markup_escape_text(&r.description))
                        .subtitle(glib::markup_escape_text(&r.id))
                        .active(r.enabled)
                        .build();
                    let st = st.clone();
                    let id = r.id.clone();
                    let guard = Rc::new(std::cell::Cell::new(false));
                    row.connect_active_notify(move |row| {
                        if guard.get() {
                            return;
                        }
                        let enabled = row.is_active();
                        let id = id.clone();
                        let st = st.clone();
                        let row = row.clone();
                        let guard = guard.clone();
                        row.set_sensitive(false);
                        glib::spawn_future_local(async move {
                            let id2 = id.clone();
                            let res = background(move || packagekit::repo_enable(id2, enabled)).await;
                            row.set_sensitive(true);
                            match res {
                                Some(Ok(())) => {
                                    st.toast(&format!("{} {}", id, if enabled { "enabled" } else { "disabled" }));
                                    st.refresh_metadata(false);
                                }
                                Some(Err(e)) => {
                                    st.toast(&e.to_string());
                                    guard.set(true);
                                    row.set_active(!enabled);
                                    guard.set(false);
                                }
                                None => {}
                            }
                        });
                    });
                    rpm_group.add(&row);
                }
            }
            Err(e) => rpm_group.add(&adw::ActionRow::builder().title("Could not load repositories").subtitle(glib::markup_escape_text(&e.to_string())).build()),
        }
        for r in remotes {
            let row = adw::SwitchRow::builder()
                .title(glib::markup_escape_text(&r.title))
                .subtitle(glib::markup_escape_text(&format!("{} · {} · {}", r.name, if r.user { "user" } else { "system" }, r.url)))
                .active(!r.disabled)
                .build();
            let st = st.clone();
            row.connect_active_notify(move |row| {
                let enabled = row.is_active();
                let (name, user) = (r.name.clone(), r.user);
                let st = st.clone();
                glib::spawn_future_local(async move {
                    match background(move || flatpak::set_remote_enabled(&name, user, enabled)).await {
                        Some(Ok(())) => {
                            st.toast(if enabled { "Remote enabled" } else { "Remote disabled" });
                            st.load_catalog();
                        }
                        Some(Err(e)) => st.toast(&e.to_string()),
                        None => {}
                    }
                });
            });
            fp_group.add(&row);
        }
    });
    dialog.present(Some(parent));
}

pub fn choose_file(parent: &gtk::Widget, state: &Rc<State>, patterns: &[&str], name: &str) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(name));
    for p in patterns {
        filter.add_pattern(p);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let fd = gtk::FileDialog::builder().title("Open Software File").filters(&filters).default_filter(&filter).modal(true).build();
    let win = parent.root().and_downcast::<gtk::Window>();
    let st = state.clone();
    let p = parent.clone();
    fd.open(win.as_ref(), gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res
            && let Some(path) = file.path() {
                open_file(&p, &st, &path.to_string_lossy());
            }
    });
}

/// Confirm and install a local .rpm / .flatpakref / .flatpakrepo file.
pub fn open_file(parent: &gtk::Widget, state: &Rc<State>, path: &str) {
    let lower = path.to_lowercase();
    let st = state.clone();
    let path = path.to_string();
    let parent = parent.clone();
    glib::spawn_future_local(async move {
        let (heading, body, action, kind) = if lower.ends_with(".rpm") {
            let p2 = path.clone();
            let info = background(move || rpm::query_file(&p2)).await.flatten();
            match info {
                Some((name, evr, summary, _)) => (
                    format!("Install {name}?"),
                    format!(
                        "{summary}\n\nVersion {evr}\n\nThis package comes from a local file and is not verified by a trusted repository. It will not receive updates unless its repository is configured. Only install software you trust."
                    ),
                    "Install",
                    "rpm",
                ),
                None => {
                    st.toast("This file is not a valid RPM package");
                    return;
                }
            }
        } else if lower.ends_with(".flatpakref") {
            let name = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            (format!("Install {name}?"), "This Flatpak app reference will be installed, adding its repository if needed.".to_string(), "Install", "flatpakref")
        } else if lower.ends_with(".flatpakrepo") {
            (
                "Add Software Repository?".to_string(),
                format!("Apps from “{}” will become available to install.", std::path::Path::new(&path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()),
                "Add",
                "flatpakrepo",
            )
        } else {
            st.toast("Unsupported file type");
            return;
        };
        let d = adw::AlertDialog::builder().heading(heading).body(body).close_response("cancel").default_response("ok").build();
        d.add_response("cancel", "Cancel");
        d.add_response("ok", action);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let st2 = st.clone();
        d.connect_response(None, move |_, r| {
            if r != "ok" {
                return;
            }
            match kind {
                "rpm" => st2.install_rpm_file(path.clone()),
                "flatpakref" => st2.install_flatpakref(path.clone()),
                _ => st2.add_flatpakrepo(path.clone()),
            }
        });
        d.present(Some(&parent));
    });
}

pub fn about(parent: &impl IsA<gtk::Widget>) {
    let about = adw::AboutDialog::builder()
        .application_name(APP_NAME)
        .application_icon(APP_ID)
        .version(VERSION)
        .developer_name("jh")
        .comments("A fast software store for Fedora with RPM, Terra and Flatpak support.")
        .license_type(gtk::License::Gpl30)
        .build();
    about.add_credit_section(Some("Data Sources"), &["AppStream", "Flathub", "Terra https://terra.fyralabs.com", "Open Desktop Ratings Service https://odrs.gnome.org"]);
    about.present(Some(parent));
}
