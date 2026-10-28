//! Discover page (spotlight, curated sections) and category pages.

use super::widgets::{self, AppGrid, OpenApp};
use crate::catalog::{self, CATEGORIES};
use crate::images;
use crate::model::App;
use crate::state::{Change, State};
use adw::prelude::*;
use std::rc::Rc;
use std::sync::Arc;

pub fn build(state: &Rc<State>, open: OpenApp, open_category: Rc<dyn Fn(&'static str)>) -> gtk::Widget {
    let stack = gtk::Stack::new();
    let loading = adw::StatusPage::builder()
        .title("Loading Software Catalog…")
        .child(&{
            let s = adw::Spinner::new();
            s.set_size_request(48, 48);
            s
        })
        .build();
    stack.add_named(&loading, Some("loading"));
    let scrolled = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).build();
    stack.add_named(&scrolled, Some("content"));

    let rebuild = {
        let state = Rc::downgrade(state);
        let scrolled = scrolled.downgrade();
        let stack = stack.downgrade();
        move || {
            let (Some(state), Some(scrolled), Some(stack)) = (state.upgrade(), scrolled.upgrade(), stack.upgrade()) else { return false };
            if !state.catalog_ready.get() {
                return true;
            }
            scrolled.set_child(Some(&content(&state, &open, &open_category)));
            stack.set_visible_child_name("content");
            true
        }
    };
    rebuild();
    state.subscribe(move |c| if c == Change::Catalog { rebuild() } else { true });
    stack.upcast()
}

fn day_seed() -> u64 {
    crate::config::now() / 86400
}

/// Deterministic daily shuffle so the explore page changes every day.
fn shuffle(v: &mut [Arc<App>], seed: u64) {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let j = (x % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

fn content(state: &Rc<State>, open: &OpenApp, open_category: &Rc<dyn Fn(&'static str)>) -> gtk::Widget {
    let cat = state.catalog();
    let nonfree = state.config.borrow().show_nonfree;
    let b = gtk::Box::new(gtk::Orientation::Vertical, 18);
    b.set_margin_top(24);
    b.set_margin_bottom(36);
    b.set_margin_start(16);
    b.set_margin_end(16);

    let intro = gtk::Box::new(gtk::Orientation::Vertical, 4);
    intro.append(&gtk::Label::builder().label("Discover").xalign(0.0).css_classes(["title-1"]).build());
    intro.append(
        &gtk::Label::builder()
            .label(format!("{} apps and packages from RPM, Terra and Flatpak", cat.len()))
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    b.append(&intro);

    let mut featured = cat.featured(nonfree);
    featured.retain(|a| !a.screenshots.is_empty());
    shuffle(&mut featured, day_seed());
    if !featured.is_empty() {
        b.append(&section_header("Spotlight", "A fresh selection every day", None));
        b.append(&spotlight(&featured[..featured.len().min(4)], open));
    }

    // A few apps that are available as native packages (RPM / Terra).
    let mut native: Vec<Arc<App>> = cat
        .featured(nonfree)
        .into_iter()
        .chain(cat.popular(nonfree))
        .filter(|a| a.sources.iter().any(|s| s.kind.is_rpm()))
        .collect();
    native.dedup_by(|a, b| a.id == b.id);
    shuffle(&mut native, day_seed() + 2);
    if native.len() >= 4 {
        b.append(&section_header("Native on Fedora", "Installed as RPM packages, no sandbox runtime needed", None));
        b.append(&widgets::tile_flowbox(state, &native[..native.len().min(8)], open));
    }

    let new = cat.new_and_updated(nonfree, 8);
    if !new.is_empty() {
        b.append(&section_header("Recently Updated", "Latest releases across all sources", None));
        b.append(&widgets::tile_flowbox(state, &new, open));
    }

    let mut popular = cat.popular(nonfree);
    shuffle(&mut popular, day_seed() + 1);
    if !popular.is_empty() {
        b.append(&section_header("Worth a Look", "Well-loved apps worth trying", None));
        b.append(&widgets::tile_flowbox(state, &popular[..popular.len().min(8)], open));
    }

    for (i, c) in CATEGORIES.iter().enumerate().filter(|(_, c)| matches!(c.id, "play" | "create" | "develop")) {
        let mut apps = cat.in_category(c, nonfree);
        apps.truncate(24);
        shuffle(&mut apps, day_seed() + 10 + i as u64);
        if apps.is_empty() {
            continue;
        }
        let oc = open_category.clone();
        let id = c.id;
        b.append(&section_header(c.name, "", Some(Box::new(move || oc(id)))));
        b.append(&widgets::tile_flowbox(state, &apps[..apps.len().min(8)], open));
    }

    let clamp = adw::Clamp::builder().maximum_size(1200).child(&b).build();
    clamp.upcast()
}

/// Section title with an optional subtitle and "See All" button.
fn section_header(title: &str, subtitle: &str, see_all: Option<Box<dyn Fn()>>) -> gtk::Widget {
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.set_margin_top(12);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["title-3"]).build());
    if !subtitle.is_empty() {
        text.append(&gtk::Label::builder().label(subtitle).xalign(0.0).wrap(true).css_classes(["dim-label", "caption"]).build());
    }
    head.append(&text);
    if let Some(f) = see_all {
        let more = gtk::Button::builder().label("See All").css_classes(["flat"]).valign(gtk::Align::End).build();
        more.connect_clicked(move |_| f());
        head.append(&more);
    }
    head.upcast()
}

/// Two-column grid of large highlighted cards.
fn spotlight(apps: &[Arc<App>], open: &OpenApp) -> gtk::Widget {
    let fb = gtk::FlowBox::builder()
        .homogeneous(true)
        .min_children_per_line(1)
        .max_children_per_line(2)
        .column_spacing(12)
        .row_spacing(12)
        .selection_mode(gtk::SelectionMode::None)
        .build();
    for (i, app) in apps.iter().enumerate() {
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        hb.set_margin_top(18);
        hb.set_margin_bottom(18);
        hb.set_margin_start(18);
        hb.set_margin_end(18);
        let icon = gtk::Image::new();
        images::set_icon(&icon, app.icon.as_ref(), 64);
        icon.add_css_class("icon-dropshadow");
        icon.set_valign(gtk::Align::Center);
        hb.append(&icon);
        let tb = gtk::Box::new(gtk::Orientation::Vertical, 4);
        tb.set_valign(gtk::Align::Center);
        tb.append(&gtk::Label::builder().label(&app.name).xalign(0.0).wrap(true).max_width_chars(16).css_classes(["title-2"]).build());
        tb.append(
            &gtk::Label::builder()
                .label(&app.summary)
                .xalign(0.0)
                .wrap(true)
                .lines(2)
                .max_width_chars(24)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
        );
        hb.append(&tb);
        let btn = gtk::Button::builder()
            .child(&hb)
            .css_classes(["card", "featured-tile", &format!("featured-{}", i % 6)])
            .height_request(140)
            .build();
        let a = app.clone();
        let open = open.clone();
        btn.connect_clicked(move |_| open(a.clone()));
        fb.append(&btn);
    }
    fb.upcast()
}

/// A page listing all apps of one category, with a subcategory filter.
pub fn category_page(state: &Rc<State>, id: &'static str, open: OpenApp) -> adw::NavigationPage {
    let def = catalog::category(id).expect("valid category");
    let grid = AppGrid::new(state, open);
    let apps = state.catalog().in_category(def, state.config.borrow().show_nonfree);

    // Subcategory filter
    let mut subs: Vec<&str> = def
        .matches
        .iter()
        .copied()
        .filter(|m| apps.iter().filter(|a| a.categories.iter().any(|c| c == m)).count() >= 3)
        .collect();
    subs.sort();
    let mut labels = vec!["All".to_string()];
    labels.extend(subs.iter().map(|s| subcategory_label(s)));
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let dd = gtk::DropDown::from_strings(&label_refs);
    dd.set_visible(subs.len() > 1);

    grid.set_apps(&apps);
    let apps = Rc::new(apps);
    let g = grid.clone();
    let a = apps.clone();
    let subs_owned: Vec<String> = subs.iter().map(|s| s.to_string()).collect();
    dd.connect_selected_notify(move |dd| {
        let i = dd.selected() as usize;
        if i == 0 {
            g.set_apps(&a);
        } else if let Some(sub) = subs_owned.get(i - 1) {
            let filtered: Vec<Arc<App>> = a.iter().filter(|x| x.categories.iter().any(|c| c == sub)).cloned().collect();
            g.set_apps(&filtered);
        }
    });

    let hb = adw::HeaderBar::new();
    hb.pack_end(&dd);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&hb);
    tv.set_content(Some(&grid.widget));

    let g = Rc::downgrade(&grid);
    let page_widget = tv.downgrade();
    state.subscribe(move |c| {
        if page_widget.upgrade().is_none() {
            return false;
        }
        if c == Change::Installed
            && let Some(g) = g.upgrade() {
                g.refresh();
            }
        true
    });
    let page = adw::NavigationPage::builder().title(def.name).child(&tv).tag(format!("category:{id}")).build();
    unsafe { page.set_data("grid", grid) };
    page
}

fn subcategory_label(s: &str) -> String {
    match s {
        "AudioVideoEditing" => "Audio & Video Editing".into(),
        "3DGraphics" => "3D Graphics".into(),
        "VectorGraphics" => "Vector Graphics".into(),
        "RasterGraphics" => "Raster Graphics".into(),
        "InstantMessaging" => "Instant Messaging".into(),
        "IRCClient" => "IRC".into(),
        "VideoConference" => "Video Conferencing".into(),
        "WebBrowser" => "Web Browsers".into(),
        "ProjectManagement" => "Project Management".into(),
        "WordProcessor" => "Word Processors".into(),
        "ContactManagement" => "Contacts".into(),
        "RevisionControl" => "Version Control".into(),
        "WebDevelopment" => "Web Development".into(),
        "GUIDesigner" => "GUI Designers".into(),
        "TerminalEmulator" => "Terminals".into(),
        "TextEditor" => "Text Editors".into(),
        "FileManager" => "File Managers".into(),
        "FileTools" => "File Tools".into(),
        other => {
            // Insert spaces into CamelCase.
            let mut out = String::new();
            for (i, ch) in other.chars().enumerate() {
                if i > 0 && ch.is_uppercase() {
                    out.push(' ');
                }
                out.push(ch);
            }
            out
        }
    }
}
