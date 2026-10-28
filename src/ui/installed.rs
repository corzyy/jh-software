//! Installed apps page.

use super::details::confirm_remove;
use super::widgets::{self, OpenApp};
use crate::model::*;
use crate::state::{Change, State};
use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub fn build(state: &Rc<State>, open: OpenApp) -> gtk::Widget {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.set_margin_top(24);
    content.set_margin_bottom(36);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let filter = gtk::SearchEntry::builder().placeholder_text("Filter installed apps").build();
    content.append(&filter);
    let groups = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.append(&groups);

    let clamp = adw::Clamp::builder().maximum_size(900).child(&content).build();
    let scrolled = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).build();

    let rebuild: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    {
        let state_w = Rc::downgrade(state);
        let groups = groups.downgrade();
        let filter_w = filter.downgrade();
        *rebuild.borrow_mut() = Some(Box::new(move || {
            let (Some(state), Some(groups), Some(filter)) = (state_w.upgrade(), groups.upgrade(), filter_w.upgrade()) else { return };
            while let Some(c) = groups.first_child() {
                groups.remove(&c);
            }
            fill(&state, &groups, &open, &filter.text().to_lowercase());
        }));
    }
    let r = rebuild.clone();
    filter.connect_search_changed(move |_| {
        if let Some(f) = r.borrow().as_ref() {
            f();
        }
    });
    if let Some(f) = rebuild.borrow().as_ref() {
        f();
    }
    let weak = scrolled.downgrade();
    let last_sig = RefCell::new(state.jobs_signature());
    let st = Rc::downgrade(state);
    state.subscribe(move |c| {
        if weak.upgrade().is_none() {
            return false;
        }
        let Some(s) = st.upgrade() else { return false };
        let sig = s.jobs_signature();
        let jobs_changed = c == Change::Jobs && *last_sig.borrow() != sig;
        *last_sig.borrow_mut() = sig;
        if (jobs_changed || matches!(c, Change::Installed | Change::Catalog | Change::Updates))
            && let Some(f) = rebuild.borrow().as_ref() {
                f();
            }
        true
    });
    scrolled.upcast()
}

struct Entry {
    app: Arc<App>,
    source: Source,
}

fn collect(state: &State) -> (Vec<Entry>, Vec<Entry>) {
    let cat = state.catalog();
    let inst = state.installed.borrow();
    let mut apps = Vec::new();
    let mut other = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Flatpak apps (always listed, even without catalog metadata).
    for f in inst.flatpaks.iter().filter(|f| f.is_app) {
        let src = Source { kind: SourceKind::Flatpak, id: f.ref_.clone(), origin: f.origin.clone(), user: f.user };
        let app = match cat.by_flatpak_name(&f.name) {
            Some(a) => a.clone(),
            None => Arc::new(App {
                id: f.name.clone(),
                kind: AppKind::Desktop,
                name: f.title.clone().unwrap_or_else(|| f.name.clone()),
                summary: f.summary.clone().unwrap_or_default(),
                description: vec![],
                developer: None,
                icon: Some(Icon::Stock(f.name.clone())),
                categories: vec![],
                keywords: vec![],
                license: None,
                urls: vec![],
                screenshots: vec![],
                releases: vec![],
                content_rating: vec![],
                launchable: Some(format!("{}.desktop", f.name)),
                verified: false,
                featured: false,
                popular: false,
                extends: vec![],
                sources: vec![src.clone()],
            }),
        };
        if seen.insert((app.id.clone(), src.id.clone())) {
            apps.push(Entry { app, source: src });
        }
    }
    // RPM apps known to AppStream.
    for a in cat.apps.iter() {
        for s in a.sources.iter().filter(|s| s.kind.is_rpm()) {
            if inst.rpms.contains_key(&s.id) && seen.insert((a.id.clone(), s.id.clone())) {
                let mut s = s.clone();
                // Attribute to Terra if that's where the installed package came from.
                if let Some(r) = inst.rpms.get(&s.id)
                    && r.is_terra() {
                        s.kind = SourceKind::Terra;
                        s.origin = crate::backend::terra::REPO_ID.into();
                    }
                let e = Entry { app: a.clone(), source: s };
                match a.kind {
                    AppKind::Desktop => apps.push(e),
                    AppKind::Package => other.push(e),
                    AppKind::Addon | AppKind::Font | AppKind::Codec | AppKind::Console => other.push(e),
                    AppKind::Runtime => {}
                }
            }
        }
    }
    apps.sort_by_key(|e| e.app.name.to_lowercase());
    other.sort_by_key(|e| e.app.name.to_lowercase());
    (apps, other)
}

fn fill(state: &Rc<State>, groups: &gtk::Box, open: &OpenApp, filter: &str) {
    if !state.installed.borrow().loaded {
        let s = adw::Spinner::new();
        s.set_size_request(32, 32);
        groups.append(&s);
        return;
    }
    let (apps, other) = collect(state);
    let matches = |e: &Entry| filter.is_empty() || e.app.name.to_lowercase().contains(filter) || e.source.id.to_lowercase().contains(filter);
    let apps: Vec<&Entry> = apps.iter().filter(|e| matches(e)).collect();
    let other: Vec<&Entry> = other.iter().filter(|e| matches(e)).collect();

    let total: u64 = apps.iter().filter_map(|e| state.installed_size(&e.source)).sum();
    let g = adw::PreferencesGroup::builder()
        .title(format!("Applications ({})", apps.len()))
        .description(format!("{} used", format_size(total)))
        .build();
    let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
    for e in &apps {
        list.append(&row(state, e, open));
    }
    if apps.is_empty() {
        list.append(&adw::ActionRow::builder().title("No applications found").build());
    }
    g.add(&list);
    groups.append(&g);

    if !other.is_empty() {
        let g = adw::PreferencesGroup::builder()
            .title(format!("Add-ons, Fonts and Other Software ({})", other.len()))
            .build();
        let exp = adw::ExpanderRow::builder().title("Show other installed software").build();
        for e in &other {
            exp.add_row(&row(state, e, open));
        }
        let lb = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
        lb.append(&exp);
        g.add(&lb);
        groups.append(&g);
    }
}

fn row(state: &Rc<State>, e: &Entry, open: &OpenApp) -> adw::ActionRow {
    let mut sub = format!("{} · {}", e.source.kind.label(), e.source.display_origin());
    if let Some(sz) = state.installed_size(&e.source).filter(|s| *s > 0) {
        sub.push_str(&format!(" · {}", format_size(sz)));
    }
    if let Some(v) = state.installed_version(&e.source) {
        sub.push_str(&format!(" · {v}"));
    }
    let row = widgets::app_row(&e.app, &sub);
    if let Some(j) = state.job_for(&e.app.id) {
        let l = gtk::Label::builder().label(&j.status).css_classes(["dim-label", "caption"]).build();
        row.add_suffix(&l);
    } else {
        if state.has_update(&e.app) {
            let up = gtk::Image::from_icon_name("software-update-available-symbolic");
            up.set_tooltip_text(Some("Update available"));
            up.add_css_class("accent");
            row.add_suffix(&up);
        }
        let btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Uninstall")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        let st = state.clone();
        let app = e.app.clone();
        let src = e.source.clone();
        btn.connect_clicked(move |b| confirm_remove(b.upcast_ref(), &st, &app, src.clone()));
        row.add_suffix(&btn);
    }
    let app = e.app.clone();
    let open = open.clone();
    row.connect_activated(move |_| open(app.clone()));
    let _ = glib::markup_escape_text("");
    row
}
