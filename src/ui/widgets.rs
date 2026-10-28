//! Reusable widgets: app tiles, the virtualized app grid, rows and helpers.

use crate::images;
use crate::model::*;
use crate::state::State;
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;
use std::sync::Arc;

pub type OpenApp = Rc<dyn Fn(Arc<App>)>;

pub struct Tile {
    pub root: gtk::Button,
    icon: gtk::Image,
    name: gtk::Label,
    summary: gtk::Label,
    badge: gtk::Image,
    sources: gtk::Label,
}

impl Tile {
    pub fn new() -> Self {
        let icon = gtk::Image::builder().pixel_size(64).valign(gtk::Align::Center).build();
        icon.add_css_class("icon-dropshadow");
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(24)
            .css_classes(["heading"])
            .build();
        let badge = gtk::Image::builder().icon_name("object-select-symbolic").visible(false).build();
        badge.add_css_class("installed-badge");
        badge.set_tooltip_text(Some("Installed"));
        let name_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        name_box.append(&name);
        name_box.append(&badge);
        let summary = gtk::Label::builder()
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(2)
            .max_width_chars(30)
            .width_chars(20)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        let sources = gtk::Label::builder().xalign(0.0).css_classes(["source-label", "caption"]).build();
        let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
        text.set_valign(gtk::Align::Center);
        text.set_hexpand(true);
        text.append(&name_box);
        text.append(&summary);
        text.append(&sources);
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        hbox.set_margin_top(12);
        hbox.set_margin_bottom(12);
        hbox.set_margin_start(12);
        hbox.set_margin_end(12);
        hbox.append(&icon);
        hbox.append(&text);
        let root = gtk::Button::builder().child(&hbox).css_classes(["card", "app-tile"]).build();
        root.set_size_request(280, 100);
        Tile { root, icon, name, summary, badge, sources }
    }

    pub fn bind(&self, state: &State, app: &App) {
        images::set_icon(&self.icon, app.icon.as_ref(), 64);
        self.name.set_label(&app.name);
        self.summary.set_label(&app.summary);
        self.badge.set_visible(state.is_installed(app));
        let mut kinds: Vec<&str> = Vec::new();
        for s in state.sorted_sources(app) {
            let l = s.kind.label();
            if !kinds.contains(&l) {
                kinds.push(l);
            }
        }
        self.sources.set_label(&kinds.join(" · "));
    }
}

/// A virtualized grid of apps (fast with thousands of entries).
pub struct AppGrid {
    pub widget: gtk::ScrolledWindow,
    store: gio::ListStore,
}

impl AppGrid {
    pub fn new(state: &Rc<State>, open: OpenApp) -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let tile = Tile::new();
            // The GridView handles activation; the button is only for styling.
            tile.root.set_can_target(false);
            tile.root.set_can_focus(false);
            item.set_child(Some(&tile.root));
            unsafe { item.set_data("tile", tile) };
        });
        let s = Rc::downgrade(state);
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(state) = s.upgrade() else { return };
            let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() else { return };
            let app = obj.borrow::<Arc<App>>();
            if let Some(tile) = unsafe { item.data::<Tile>("tile") } {
                unsafe { tile.as_ref() }.bind(&state, &app);
            }
        });
        let selection = gtk::NoSelection::new(Some(store.clone()));
        let view = gtk::GridView::builder()
            .model(&selection)
            .factory(&factory)
            .min_columns(1)
            .max_columns(4)
            .single_click_activate(true)
            .css_classes(["app-grid"])
            .build();
        let st = store.clone();
        view.connect_activate(move |_, pos| {
            if let Some(obj) = st.item(pos).and_downcast::<glib::BoxedAnyObject>() {
                let app = obj.borrow::<Arc<App>>().clone();
                open(app);
            }
        });
        let clamp = adw::ClampScrollable::builder().maximum_size(1200).child(&view).build();
        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        Rc::new(AppGrid { widget, store })
    }

    pub fn set_apps(&self, apps: &[Arc<App>]) {
        let objs: Vec<glib::BoxedAnyObject> = apps.iter().map(|a| glib::BoxedAnyObject::new(a.clone())).collect();
        self.store.splice(0, self.store.n_items(), &objs);
        self.widget.vadjustment().set_value(0.0);
    }

    /// Re-bind every visible tile (installed badges changed).
    pub fn refresh(&self) {
        let n = self.store.n_items();
        if n > 0 {
            self.store.items_changed(0, n, n);
        }
    }
}

/// Wrapping box of tiles for small, fixed sets (explore page sections).
pub fn tile_flowbox(state: &Rc<State>, apps: &[Arc<App>], open: &OpenApp) -> gtk::FlowBox {
    let fb = gtk::FlowBox::builder()
        .homogeneous(true)
        .min_children_per_line(1)
        .max_children_per_line(4)
        .column_spacing(12)
        .row_spacing(12)
        .selection_mode(gtk::SelectionMode::None)
        .build();
    for app in apps {
        let tile = Tile::new();
        tile.bind(state, app);
        let a = app.clone();
        let open = open.clone();
        tile.root.connect_clicked(move |_| open(a.clone()));
        fb.append(&tile.root);
    }
    fb
}

/// Render AppStream description blocks.
pub fn description_widget(blocks: &[DescBlock]) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 10);
    for block in blocks {
        match block {
            DescBlock::Para(p) => {
                let l = gtk::Label::builder().label(p).wrap(true).xalign(0.0).selectable(false).build();
                l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                b.append(&l);
            }
            DescBlock::List { ordered, items } => {
                let lb = gtk::Box::new(gtk::Orientation::Vertical, 4);
                for (i, it) in items.iter().enumerate() {
                    let bullet = if *ordered { format!("{}.", i + 1) } else { "•".to_string() };
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    row.set_margin_start(8);
                    row.append(&gtk::Label::builder().label(bullet).yalign(0.0).build());
                    let l = gtk::Label::builder().label(it).wrap(true).xalign(0.0).hexpand(true).build();
                    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                    row.append(&l);
                    lb.append(&row);
                }
                b.append(&lb);
            }
        }
    }
    b
}

pub fn section_title(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).css_classes(["title-3"]).margin_top(12).build()
}

pub fn format_date(ts: i64) -> String {
    glib::DateTime::from_unix_local(ts)
        .and_then(|d| d.format("%x"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

pub fn relative_time(ts: u64) -> String {
    if ts == 0 {
        return "never".into();
    }
    let d = crate::config::now().saturating_sub(ts);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86400),
    }
}

/// Icon + name row used in lists (installed, updates).
pub fn app_row(app: &App, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&app.name))
        .subtitle(glib::markup_escape_text(subtitle))
        .activatable(true)
        .build();
    let img = gtk::Image::new();
    images::set_icon(&img, app.icon.as_ref(), 48);
    img.add_css_class("icon-dropshadow");
    row.add_prefix(&img);
    row
}

/// Approximate minimum age from OARS content rating attributes.
pub fn age_rating(attrs: &[(String, String)]) -> Option<u32> {
    if attrs.is_empty() {
        return None;
    }
    let mut age = 0;
    for (id, value) in attrs {
        let lvl = match value.as_str() {
            "mild" => 1,
            "moderate" => 2,
            "intense" => 3,
            _ => 0,
        };
        if lvl == 0 {
            continue;
        }
        let table: [u32; 3] = match id.as_str() {
            "violence-cartoon" => [3, 4, 6],
            "violence-fantasy" => [3, 7, 8],
            "violence-realistic" => [4, 9, 14],
            "violence-bloodshed" => [9, 11, 18],
            "violence-sexual" | "sex-nudity" | "sex-themes" => [12, 14, 18],
            "drugs-alcohol" | "drugs-narcotics" | "drugs-tobacco" => [11, 13, 16],
            "language-profanity" | "language-humor" => [8, 11, 14],
            "language-discrimination" => [9, 10, 11],
            "money-gambling" => [7, 10, 18],
            "money-purchasing" | "money-advertising" => [3, 3, 3],
            "social-chat" | "social-info" | "social-audio" | "social-location" | "social-contacts" => [4, 10, 13],
            _ => [3, 8, 13],
        };
        age = age.max(table[lvl - 1]);
    }
    Some(age)
}

pub fn stars_text(avg: f64) -> String {
    let full = avg.round() as usize;
    let mut s = String::new();
    for i in 0..5 {
        s.push(if i < full { '★' } else { '☆' });
    }
    s
}

pub fn spinner() -> gtk::Widget {
    adw::Spinner::new().upcast()
}
