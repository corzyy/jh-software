//! Updates page: flatpak app updates, RPM app updates and system (OS) updates.

use super::widgets::{self, OpenApp};
use crate::backend::packagekit::Package;
use crate::state::{Change, State};
use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

pub fn build(state: &Rc<State>, open: OpenApp) -> (gtk::Widget, gtk::Button) {
    let check_btn = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Check for Updates").build();
    let st = Rc::downgrade(state);
    check_btn.connect_clicked(move |_| {
        if let Some(s) = st.upgrade() {
            s.refresh_metadata(true);
        }
    });

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.set_vexpand(true);
    let bars: Rc<RefCell<Bars>> = Rc::new(RefCell::new(Vec::new()));
    let rebuild = {
        let state = Rc::downgrade(state);
        let holder = holder.downgrade();
        let check_btn = check_btn.downgrade();
        let bars = bars.clone();
        move || -> bool {
            let (Some(state), Some(holder), Some(cb)) = (state.upgrade(), holder.upgrade(), check_btn.upgrade()) else { return false };
            let busy = state.updates.borrow().checking || state.refreshing.get();
            cb.set_sensitive(!busy);
            while let Some(c) = holder.first_child() {
                holder.remove(&c);
            }
            bars.borrow_mut().clear();
            holder.append(&content(&state, &open, &mut bars.borrow_mut()));
            true
        }
    };
    rebuild();
    // Only rebuild when the set of jobs changes; progress ticks update bars in place.
    let last_sig = RefCell::new(state.jobs_signature());
    let st = Rc::downgrade(state);
    state.subscribe(move |c| match c {
        Change::Updates | Change::Installed | Change::Catalog => {
            if let Some(s) = st.upgrade() {
                *last_sig.borrow_mut() = s.jobs_signature();
            }
            rebuild()
        }
        Change::Jobs => {
            let Some(s) = st.upgrade() else { return false };
            let sig = s.jobs_signature();
            if *last_sig.borrow() != sig {
                *last_sig.borrow_mut() = sig;
                return rebuild();
            }
            for (id, pb, label) in bars.borrow().iter() {
                if let Some(j) = s.jobs().into_iter().find(|j| j.id == *id) {
                    match j.fraction {
                        Some(f) => pb.set_fraction(f),
                        None => pb.pulse(),
                    }
                    if let Some(l) = label {
                        l.set_label(&j.status);
                    }
                }
            }
            true
        }
    });
    (holder.upcast(), check_btn)
}

type Bars = Vec<(u64, gtk::ProgressBar, Option<gtk::Label>)>;

fn content(state: &Rc<State>, open: &OpenApp, bars: &mut Bars) -> gtk::Widget {
    let u = state.updates.borrow();
    let busy = u.checking || state.refreshing.get();
    let jobs = state.jobs();

    if u.count() == 0 && !u.offline_pending {
        let status = adw::StatusPage::builder().vexpand(true).build();
        if busy {
            status.set_title("Checking for Updates…");
            let s = adw::Spinner::new();
            s.set_size_request(48, 48);
            status.set_child(Some(&s));
        } else if let Some(e) = &u.error {
            status.set_icon_name(Some("dialog-warning-symbolic"));
            status.set_title("Could Not Check for Updates");
            status.set_description(Some(&glib::markup_escape_text(e)));
        } else {
            status.set_icon_name(Some("object-select-symbolic"));
            status.set_title("Up to Date");
            status.set_description(Some(&format!("Last checked: {}", widgets::relative_time(u.checked))));
        }
        return status.upcast();
    }

    let b = gtk::Box::new(gtk::Orientation::Vertical, 24);
    b.set_margin_top(24);
    b.set_margin_bottom(36);
    b.set_margin_start(16);
    b.set_margin_end(16);

    if busy {
        let l = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        l.set_halign(gtk::Align::Center);
        l.append(&widgets::spinner());
        l.append(&gtk::Label::new(Some("Checking for updates…")));
        b.append(&l);
    }

    // Running update jobs
    for j in jobs.iter().filter(|j| j.app_id.is_none()) {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("card");
        card.add_css_class("padded-card");
        card.append(&gtk::Label::builder().label(&j.title).xalign(0.0).css_classes(["heading"]).build());
        let pb = gtk::ProgressBar::new();
        match j.fraction {
            Some(f) => pb.set_fraction(f),
            None => pb.pulse(),
        }
        card.append(&pb);
        let status = gtk::Label::builder().label(&j.status).xalign(0.0).css_classes(["caption", "dim-label"]).build();
        card.append(&status);
        bars.push((j.id, pb, Some(status)));
        b.append(&card);
    }

    if u.offline_pending {
        let banner = adw::PreferencesGroup::new();
        let row = adw::ActionRow::builder()
            .title("System updates are ready")
            .subtitle("They will be installed when you restart the computer")
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("system-reboot-symbolic"));
        let restart = gtk::Button::builder().label("Restart & Install").valign(gtk::Align::Center).css_classes(["suggested-action"]).build();
        restart.connect_clicked(move |b| {
            if let Err(e) = crate::backend::packagekit::request_reboot() {
                let d = adw::AlertDialog::new(Some("Could Not Restart"), Some(&e.to_string()));
                d.add_response("ok", "OK");
                d.present(Some(b));
            }
        });
        let cancel = gtk::Button::builder().label("Cancel").valign(gtk::Align::Center).build();
        let st = state.clone();
        cancel.connect_clicked(move |_| st.cancel_offline_update());
        row.add_suffix(&cancel);
        row.add_suffix(&restart);
        banner.add(&row);
        b.append(&banner);
    }

    let cat = state.catalog();

    // ---- Flatpak updates ---------------------------------------------------
    let fp_apps: Vec<_> = u.flatpak.iter().filter(|f| f.is_app).cloned().collect();
    let fp_runtimes: Vec<_> = u.flatpak.iter().filter(|f| !f.is_app).cloned().collect();
    if !u.flatpak.is_empty() {
        let g = adw::PreferencesGroup::builder()
            .title("Flatpak Apps")
            .description(format!("{} updates", u.flatpak.len()))
            .build();
        let all = gtk::Button::builder().label("Update All").css_classes(["suggested-action"]).valign(gtk::Align::Center).build();
        let st = state.clone();
        all.connect_clicked(move |_| st.update_flatpaks(None));
        all.set_sensitive(!jobs.iter().any(|j| j.title.contains("Flatpak")));
        g.set_header_suffix(Some(&all));
        for f in &fp_apps {
            let app = cat.by_flatpak_name(&f.name).cloned();
            let title = app.as_ref().map(|a| a.name.clone()).or_else(|| f.title.clone()).unwrap_or_else(|| f.name.clone());
            let sub = format!("{} · {}", crate::model::pretty_origin(&f.origin), if f.user { "user" } else { "system" });
            let row = match &app {
                Some(a) => widgets::app_row(a, &sub),
                None => adw::ActionRow::builder().title(glib::markup_escape_text(&title)).subtitle(sub).build(),
            };
            let job = app.as_ref().and_then(|a| state.job_for(&a.id));
            if let Some(j) = job {
                let pb = gtk::ProgressBar::builder().valign(gtk::Align::Center).width_request(120).build();
                if let Some(fr) = j.fraction {
                    pb.set_fraction(fr);
                }
                bars.push((j.id, pb.clone(), None));
                row.add_suffix(&pb);
            } else {
                let btn = gtk::Button::builder().label("Update").valign(gtk::Align::Center).build();
                let st = state.clone();
                let one = f.clone();
                btn.connect_clicked(move |_| st.update_flatpaks(Some(vec![one.clone()])));
                row.add_suffix(&btn);
            }
            if let Some(a) = app {
                let open = open.clone();
                row.set_activatable(true);
                row.connect_activated(move |_| open(a.clone()));
            }
            g.add(&row);
        }
        if !fp_runtimes.is_empty() {
            let exp = adw::ExpanderRow::builder()
                .title(format!("Runtimes and Extensions ({})", fp_runtimes.len()))
                .subtitle("Shared components used by Flatpak apps")
                .build();
            exp.add_prefix(&gtk::Image::from_icon_name("application-x-addon-symbolic"));
            for f in &fp_runtimes {
                exp.add_row(&adw::ActionRow::builder().title(glib::markup_escape_text(&f.name)).subtitle(glib::markup_escape_text(&f.ref_)).build());
            }
            g.add(&exp);
        }
        b.append(&g);
    }

    // ---- RPM updates ---------------------------------------------------------
    if !u.rpm.is_empty() {
        let (app_pkgs, sys_pkgs): (Vec<&Package>, Vec<&Package>) = u
            .rpm
            .iter()
            .partition(|p| cat.by_package(p.name()).is_some_and(|a| a.kind == crate::model::AppKind::Desktop));
        let security = u.rpm.iter().filter(|p| p.is_security()).count();
        let mut desc = format!("{} packages", u.rpm.len());
        if security > 0 {
            desc.push_str(&format!(", {security} security"));
        }
        let g = adw::PreferencesGroup::builder().title("System &amp; RPM Packages").description(desc).build();

        let offline_job = jobs.iter().any(|j| j.title == "Downloading system updates");
        let restart = gtk::Button::builder().label("Download & Restart").css_classes(["suggested-action"]).valign(gtk::Align::Center).build();
        restart.set_tooltip_text(Some("Download updates and install them safely during a restart"));
        restart.set_sensitive(!offline_job);
        let st = state.clone();
        restart.connect_clicked(move |_| st.prepare_offline_update(true));
        let menu = gtk::gio::Menu::new();
        menu.append(Some("Download Only (Install on Next Restart)"), Some("app.offline-update"));
        menu.append(Some("Update Now Without Restart"), Some("app.live-update"));
        let more = gtk::MenuButton::builder().icon_name("view-more-symbolic").menu_model(&menu).valign(gtk::Align::Center).css_classes(["flat"]).build();
        let hdr = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        hdr.append(&restart);
        hdr.append(&more);
        g.set_header_suffix(Some(&hdr));

        for p in &app_pkgs {
            let Some(app) = cat.by_package(p.name()).cloned() else { continue };
            let row = widgets::app_row(&app, &format!("{} · {}", p.version(), crate::model::pretty_origin(p.repo())));
            if p.is_security() {
                let s = gtk::Image::from_icon_name("security-high-symbolic");
                s.set_tooltip_text(Some("Security update"));
                row.add_suffix(&s);
            }
            if let Some(j) = state.job_for(&app.id) {
                row.add_suffix(&gtk::Label::builder().label(&j.status).css_classes(["caption", "dim-label"]).build());
            } else {
                let btn = gtk::Button::builder().label("Update").valign(gtk::Align::Center).build();
                let st = state.clone();
                let id = p.id.clone();
                let aid = app.id.clone();
                btn.connect_clicked(move |_| st.update_rpms_live(vec![id.clone()], Some(aid.clone())));
                row.add_suffix(&btn);
            }
            let open = open.clone();
            row.connect_activated(move |_| open(app.clone()));
            g.add(&row);
        }
        if !sys_pkgs.is_empty() {
            let exp = adw::ExpanderRow::builder()
                .title("Operating System Updates")
                .subtitle(format!("{} packages", sys_pkgs.len()))
                .build();
            exp.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            for p in sys_pkgs.iter().take(500) {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(p.name()))
                    .subtitle(glib::markup_escape_text(&format!("{} · {}", p.version(), p.summary)))
                    .build();
                if p.is_security() {
                    row.add_suffix(&gtk::Image::from_icon_name("security-high-symbolic"));
                }
                exp.add_row(&row);
            }
            g.add(&exp);
        }
        b.append(&g);
    }

    let foot = gtk::Label::builder()
        .label(format!("Last checked: {}", widgets::relative_time(u.checked)))
        .css_classes(["dim-label", "caption"])
        .build();
    b.append(&foot);

    let clamp = adw::Clamp::builder().maximum_size(900).child(&b).build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build()
        .upcast()
}
