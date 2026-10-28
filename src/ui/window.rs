//! Main window: navigation, view switcher, search and jobs indicator.

use super::widgets::{AppGrid, OpenApp};
use super::{details, dialogs, explore, installed, updates};
use crate::catalog::CATEGORIES;
use crate::config::APP_NAME;
use crate::model::App;
use crate::state::{Change, State};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub struct Window {
    pub win: adw::ApplicationWindow,
    split: adw::OverlaySplitView,
    sidebar: gtk::ListBox,
    nav: adw::NavigationView,
    stack: gtk::Stack,
    search_entry: gtk::SearchEntry,
    state: Rc<State>,
    open: RefCell<Option<OpenApp>>,
    pending_app: RefCell<Option<String>>,
}

/// Top-level views shown in the sidebar: (stack name, title, icon).
const VIEWS: &[(&str, &str, &str)] = &[
    ("explore", "Discover", "view-app-grid-symbolic"),
    ("installed", "Library", "folder-download-symbolic"),
    ("updates", "Updates", "software-update-available-symbolic"),
];

impl Window {
    pub fn new(app: &adw::Application, state: &Rc<State>) -> Rc<Self> {
        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title(APP_NAME)
            .default_width(1100)
            .default_height(760)
            .width_request(360)
            .height_request(294)
            .build();

        let toast_overlay = adw::ToastOverlay::new();
        let nav = adw::NavigationView::new();
        let split = adw::OverlaySplitView::builder().content(&nav).min_sidebar_width(220.0).max_sidebar_width(260.0).build();
        toast_overlay.set_child(Some(&split));
        win.set_content(Some(&toast_overlay));

        let w = Rc::new(Window {
            win: win.clone(),
            split,
            sidebar: gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build(),
            nav: nav.clone(),
            stack: gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).transition_duration(100).build(),
            search_entry: gtk::SearchEntry::builder().placeholder_text("Search apps and packages").hexpand(true).build(),
            state: state.clone(),
            open: RefCell::new(None),
            pending_app: RefCell::new(None),
        });

        // Opening an app pushes its details page.
        let weak = Rc::downgrade(&w);
        let open: OpenApp = Rc::new(move |app: Arc<App>| {
            if let Some(w) = weak.upgrade() {
                w.show_details(app);
            }
        });
        *w.open.borrow_mut() = Some(open.clone());

        // Toasts
        let to = toast_overlay.downgrade();
        *state.toast.borrow_mut() = Some(Box::new(move |msg: &str| {
            if let Some(t) = to.upgrade() {
                let toast = adw::Toast::new(&glib::markup_escape_text(msg));
                toast.set_timeout(4);
                t.add_toast(toast);
            }
        }));

        w.build_main_page(open);
        w
    }

    fn build_main_page(self: &Rc<Self>, open: OpenApp) {
        let state = &self.state;
        let stack = &self.stack;

        let weak = Rc::downgrade(self);
        let open_cat: Rc<dyn Fn(&'static str)> = Rc::new(move |id| {
            if let Some(w) = weak.upgrade() {
                w.show_category(id);
            }
        });

        stack.add_named(&explore::build(state, open.clone(), open_cat), Some("explore"));
        stack.add_named(&installed::build(state, open.clone()), Some("installed"));
        let (updates_widget, check_btn) = updates::build(state, open.clone());
        stack.add_named(&updates_widget, Some("updates"));
        stack.set_visible_child_name("explore");

        // ---- Sidebar ---------------------------------------------------------
        let updates_badge = gtk::Label::builder().css_classes(["sidebar-badge", "numeric"]).valign(gtk::Align::Center).visible(false).build();
        for (name, title, icon) in VIEWS {
            let (row, b) = sidebar_row(title, icon);
            if *name == "updates" {
                b.append(&updates_badge);
            }
            self.sidebar.append(&row);
        }
        let heading = gtk::Label::builder().label("Categories").xalign(0.0).css_classes(["heading", "dim-label"]).margin_top(12).build();
        self.sidebar.append(&gtk::ListBoxRow::builder().child(&heading).selectable(false).activatable(false).build());
        for c in CATEGORIES {
            self.sidebar.append(&sidebar_row(c.name, c.icon).0);
        }
        let weak = Rc::downgrade(self);
        self.sidebar.connect_row_activated(move |_, row| {
            let Some(w) = weak.upgrade() else { return };
            let i = row.index() as usize;
            if let Some((name, ..)) = VIEWS.get(i) {
                w.show_view(name);
            } else if let Some(c) = i.checked_sub(VIEWS.len() + 1).and_then(|i| CATEGORIES.get(i)) {
                w.show_category(c.id);
            }
            if w.split.is_collapsed() {
                w.split.set_show_sidebar(false);
            }
        });

        // Updates badge
        let st = Rc::downgrade(state);
        state.subscribe(move |c| {
            let Some(s) = st.upgrade() else { return false };
            if matches!(c, Change::Updates) {
                let u = s.updates.borrow();
                let n = u.count();
                updates_badge.set_label(&if n > 0 { n.to_string() } else { "•".to_string() });
                updates_badge.set_visible(n > 0 || u.offline_pending);
            }
            true
        });

        let menu = gio::Menu::new();
        let s1 = gio::Menu::new();
        s1.append(Some("Refresh"), Some("app.refresh"));
        s1.append(Some("Software Repositories"), Some("app.repos"));
        s1.append(Some("Install from File…"), Some("app.install-file"));
        menu.append_section(None, &s1);
        let s2 = gio::Menu::new();
        s2.append(Some("Preferences"), Some("app.preferences"));
        s2.append(Some("Keyboard Shortcuts"), Some("win.show-help-overlay"));
        s2.append(Some("About JH Software"), Some("app.about"));
        menu.append_section(None, &s2);
        let menu_btn = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).primary(true).tooltip_text("Main Menu").build();

        let side_header = adw::HeaderBar::new();
        side_header.set_title_widget(Some(&adw::WindowTitle::new(APP_NAME, "")));
        side_header.pack_end(&menu_btn);
        let side_tv = adw::ToolbarView::new();
        side_tv.add_top_bar(&side_header);
        side_tv.set_content(Some(&gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&self.sidebar).build()));
        self.split.set_sidebar(Some(&adw::NavigationPage::builder().title(APP_NAME).child(&side_tv).build()));

        // ---- Search results --------------------------------------------------
        let search_grid = AppGrid::new(state, open.clone());
        let no_results = adw::StatusPage::builder()
            .icon_name("system-search-symbolic")
            .title("No Results Found")
            .description("Try a different search term")
            .build();
        let content_stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).transition_duration(100).build();
        content_stack.add_named(stack, Some("browse"));
        content_stack.add_named(&search_grid.widget, Some("search"));
        content_stack.add_named(&no_results, Some("noresults"));

        let weak = Rc::downgrade(self);
        let cs = content_stack.downgrade();
        let sg = search_grid.clone();
        self.search_entry.connect_search_changed(move |e| {
            let (Some(w), Some(cs)) = (weak.upgrade(), cs.upgrade()) else { return };
            let text = e.text();
            if text.trim().is_empty() {
                cs.set_visible_child_name("browse");
                w.sync_sidebar();
                return;
            }
            // Typing anywhere returns to the main page to show results.
            w.nav.pop_to_tag("main");
            let include_pkgs = w.state.config.borrow().show_packages;
            let results = w.state.catalog().search(&text, include_pkgs, 400);
            sg.set_apps(&results);
            cs.set_visible_child_name(if results.is_empty() { "noresults" } else { "search" });
            w.sidebar.unselect_all();
        });
        let sg = search_grid.clone();
        state.subscribe(move |c| {
            if c == Change::Installed {
                sg.refresh();
            }
            true
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_activate(move |_| {
            // Enter opens the top result.
            if let Some(w) = weak.upgrade() {
                let text = w.search_entry.text();
                let include = w.state.config.borrow().show_packages;
                if let Some(a) = w.state.catalog().search(&text, include, 1).into_iter().next() {
                    w.show_details(a);
                }
            }
        });
        self.search_entry.set_key_capture_widget(Some(&self.win));

        // ---- Content header --------------------------------------------------
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::Clamp::builder().maximum_size(480).child(&self.search_entry).build()));
        let toggle = gtk::ToggleButton::builder().icon_name("sidebar-show-symbolic").tooltip_text("Show Sidebar").build();
        self.split.bind_property("show-sidebar", &toggle, "active").bidirectional().sync_create().build();
        self.split.bind_property("collapsed", &toggle, "visible").sync_create().build();
        header.pack_start(&toggle);
        header.pack_end(&self.jobs_button());
        header.pack_end(&check_btn);
        // The update check button only makes sense on the Updates page.
        let cb = check_btn.clone();
        stack.connect_visible_child_name_notify(move |s| cb.set_visible(s.visible_child_name().as_deref() == Some("updates")));
        check_btn.set_visible(false);

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&content_stack));

        // Narrow layout: the sidebar becomes an overlay.
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            760.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&self.split, "collapsed", Some(&true.to_value()));
        self.win.add_breakpoint(bp);

        let page = adw::NavigationPage::builder().title(APP_NAME).tag("main").child(&tv).build();
        self.nav.add(&page);
        unsafe { page.set_data("search-grid", search_grid) };

        // Keep the sidebar selection in sync with what is shown.
        let weak = Rc::downgrade(self);
        self.nav.connect_visible_page_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                w.sync_sidebar();
            }
        });
        self.sync_sidebar();

        // Open a details page requested before the catalog finished loading.
        let weak = Rc::downgrade(self);
        state.subscribe(move |c| {
            let Some(w) = weak.upgrade() else { return false };
            if c == Change::Catalog {
                let pending = w.pending_app.borrow_mut().take();
                if let Some(id) = pending {
                    w.show_app_id(&id);
                }
            }
            true
        });

        // Keyboard shortcuts
        let weak = Rc::downgrade(self);
        let find = gio::SimpleAction::new("search", None);
        find.connect_activate(move |_, _| {
            if let Some(w) = weak.upgrade() {
                w.nav.pop_to_tag("main");
                w.search_entry.grab_focus();
            }
        });
        self.win.add_action(&find);
        let nav = self.nav.clone();
        let back = gio::SimpleAction::new("back", None);
        back.connect_activate(move |_, _| {
            nav.pop();
        });
        self.win.add_action(&back);
        for (i, (name, ..)) in VIEWS.iter().enumerate() {
            let a = gio::SimpleAction::new(&format!("view-{}", i + 1), None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    w.show_view(name);
                }
            });
            self.win.add_action(&a);
        }
        #[allow(deprecated)]
        self.win.set_help_overlay(Some(&shortcuts_window()));
    }

    fn show_view(&self, name: &str) {
        self.nav.pop_to_tag("main");
        self.search_entry.set_text("");
        self.stack.set_visible_child_name(name);
        self.sync_sidebar();
    }

    fn show_category(&self, id: &'static str) {
        self.nav.pop_to_tag("main");
        self.search_entry.set_text("");
        let open = self.open.borrow().clone().unwrap();
        self.nav.push(&explore::category_page(&self.state, id, open));
    }

    /// Highlight the sidebar row matching the visible view or category page.
    fn sync_sidebar(&self) {
        let Some(tag) = self.nav.visible_page().and_then(|p| p.tag()) else { return };
        let index = if tag == "main" {
            let current = self.stack.visible_child_name();
            VIEWS.iter().position(|(name, ..)| current.as_deref() == Some(*name))
        } else if let Some(id) = tag.strip_prefix("category:") {
            CATEGORIES.iter().position(|c| c.id == id).map(|i| i + VIEWS.len() + 1)
        } else {
            return;
        };
        match index.and_then(|i| self.sidebar.row_at_index(i as i32)) {
            Some(row) => self.sidebar.select_row(Some(&row)),
            None => self.sidebar.unselect_all(),
        }
    }

    fn jobs_button(self: &Rc<Self>) -> gtk::MenuButton {
        let spinner = adw::Spinner::new();
        let btn = gtk::MenuButton::builder().child(&spinner).tooltip_text("Running Operations").visible(false).build();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        list.set_margin_top(12);
        list.set_margin_bottom(12);
        list.set_margin_start(12);
        list.set_margin_end(12);
        list.set_width_request(320);
        let pop = gtk::Popover::builder().child(&list).build();
        btn.set_popover(Some(&pop));
        let st = Rc::downgrade(&self.state);
        let b = btn.downgrade();
        let bars: RefCell<Vec<(u64, gtk::ProgressBar, gtk::Label)>> = RefCell::new(Vec::new());
        let last: RefCell<Vec<(u64, bool)>> = RefCell::new(Vec::new());
        self.state.subscribe(move |c| {
            if c != Change::Jobs {
                return true;
            }
            let (Some(state), Some(btn)) = (st.upgrade(), b.upgrade()) else { return false };
            let jobs = state.jobs();
            btn.set_visible(!jobs.is_empty());
            let sig = state.jobs_signature();
            if *last.borrow() != sig {
                *last.borrow_mut() = sig;
                while let Some(ch) = list.first_child() {
                    list.remove(&ch);
                }
                let mut bs = bars.borrow_mut();
                bs.clear();
                for j in &jobs {
                    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
                    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    top.append(&gtk::Label::builder().label(&j.title).xalign(0.0).hexpand(true).css_classes(["heading"]).ellipsize(gtk::pango::EllipsizeMode::End).build());
                    if j.running {
                        let cancel = gtk::Button::builder().icon_name("process-stop-symbolic").css_classes(["flat", "circular"]).tooltip_text("Cancel").build();
                        let s2 = Rc::downgrade(&state);
                        let id = j.id;
                        cancel.connect_clicked(move |_| {
                            if let Some(s) = s2.upgrade() {
                                s.cancel_job(id);
                            }
                        });
                        top.append(&cancel);
                    }
                    b.append(&top);
                    let pb = gtk::ProgressBar::new();
                    let l = gtk::Label::builder().xalign(0.0).css_classes(["caption", "dim-label"]).ellipsize(gtk::pango::EllipsizeMode::End).build();
                    b.append(&pb);
                    b.append(&l);
                    list.append(&b);
                    bs.push((j.id, pb, l));
                }
            }
            for (id, pb, l) in bars.borrow().iter() {
                if let Some(j) = jobs.iter().find(|j| j.id == *id) {
                    match j.fraction {
                        Some(f) => pb.set_fraction(f),
                        None => pb.pulse(),
                    }
                    l.set_label(if j.running { &j.status } else { "Waiting in queue…" });
                }
            }
            true
        });
        btn
    }

    pub fn show_details(self: &Rc<Self>, app: Arc<App>) {
        // Reuse an existing page for the same app.
        if let Some(existing) = self.nav.find_page(&app.id) {
            self.nav.pop_to_page(&existing);
            return;
        }
        let open = self.open.borrow().clone().unwrap();
        self.nav.push(&details::build(&self.state, app, open));
    }

    pub fn show_app_id(self: &Rc<Self>, id: &str) {
        if !self.state.catalog_ready.get() {
            *self.pending_app.borrow_mut() = Some(id.to_string());
            return;
        }
        let cat = self.state.catalog();
        let app = cat.get(id).or_else(|| cat.by_flatpak_name(id)).or_else(|| cat.by_package(id)).cloned();
        match app {
            Some(a) => self.show_details(a),
            None => self.state.toast(&format!("“{id}” was not found in the software catalog")),
        }
    }

    pub fn show_updates(&self) {
        self.show_view("updates");
    }

    pub fn search(&self, text: &str) {
        self.nav.pop_to_tag("main");
        self.search_entry.set_text(text);
        self.search_entry.grab_focus();
    }

    pub fn open_file(&self, path: &str) {
        dialogs::open_file(self.win.upcast_ref(), &self.state, path);
    }
}

fn sidebar_row(title: &str, icon: &str) -> (gtk::ListBoxRow, gtk::Box) {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.append(&gtk::Image::from_icon_name(icon));
    b.append(&gtk::Label::builder().label(title).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build());
    (gtk::ListBoxRow::builder().child(&b).build(), b)
}

#[allow(deprecated)]
fn shortcuts_window() -> gtk::ShortcutsWindow {
    let ui = r#"<?xml version="1.0" encoding="UTF-8"?>
<interface>
  <object class="GtkShortcutsWindow" id="help_overlay">
    <property name="modal">True</property>
    <child>
      <object class="GtkShortcutsSection">
        <property name="section-name">shortcuts</property>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">General</property>
            <child><object class="GtkShortcutsShortcut"><property name="title">Search</property><property name="accelerator">&lt;Primary&gt;f</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Refresh</property><property name="accelerator">&lt;Primary&gt;r</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Preferences</property><property name="accelerator">&lt;Primary&gt;comma</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Keyboard Shortcuts</property><property name="accelerator">&lt;Primary&gt;question</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Quit</property><property name="accelerator">&lt;Primary&gt;q</property></object></child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Navigation</property>
            <child><object class="GtkShortcutsShortcut"><property name="title">Discover</property><property name="accelerator">&lt;Alt&gt;1</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Library</property><property name="accelerator">&lt;Alt&gt;2</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Updates</property><property name="accelerator">&lt;Alt&gt;3</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Go Back</property><property name="accelerator">&lt;Alt&gt;Left</property></object></child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>"#;
    let b = gtk::Builder::from_string(ui);
    b.object("help_overlay").expect("shortcuts window")
}
