//! App details page: header with install/open/remove and source selector,
//! a main column (screenshots, description, release notes, add-ons, reviews)
//! and a side column (at a glance, details, links, permissions), followed by
//! more apps by the developer.

use super::widgets::{self, OpenApp};
use crate::backend::{flatpak, packagekit};
use crate::images;
use crate::model::*;
use crate::odrs;
use crate::state::{background, Change, JobKind, State};
use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

struct Widgets {
    install: gtk::Button,
    open: gtk::Button,
    update: gtk::Button,
    remove: gtk::Button,
    progress_box: gtk::Box,
    progress: gtk::ProgressBar,
    progress_label: gtk::Label,
    cancel: gtk::Button,
    source_dd: gtk::DropDown,
    source_label: gtk::Label,
    version_row: adw::ActionRow,
    installed_size_row: adw::ActionRow,
    download_size_row: adw::ActionRow,
    origin_row: adw::ActionRow,
    package_row: adw::ActionRow,
    size_tile: (gtk::Label, gtk::Label),
    safety_tile: (gtk::Image, gtk::Label, gtk::Label),
    source_tile: (gtk::Label, gtk::Label),
    permissions: adw::PreferencesGroup,
    permission_rows: RefCell<Vec<adw::ActionRow>>,
    addons: adw::PreferencesGroup,
    addon_rows: RefCell<Vec<(Arc<App>, gtk::Button)>>,
}

struct Page {
    state: Rc<State>,
    app: Arc<App>,
    sources: RefCell<Vec<Source>>,
    selected: RefCell<Option<Source>>,
    w: Widgets,
    updating_dd: Cell<bool>,
    info_generation: Cell<u64>,
}

pub fn build(state: &Rc<State>, app: Arc<App>, open: OpenApp) -> adw::NavigationPage {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.set_margin_top(24);
    content.set_margin_bottom(36);
    content.set_margin_start(16);
    content.set_margin_end(16);

    // ---- Header --------------------------------------------------------
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    let icon = gtk::Image::new();
    images::set_icon(&icon, app.icon.as_ref(), 128);
    icon.add_css_class("icon-dropshadow");
    icon.set_valign(gtk::Align::Center);
    header.append(&icon);

    let title_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    title_box.set_valign(gtk::Align::Center);
    title_box.set_hexpand(true);
    let name = gtk::Label::builder()
        .label(&app.name)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .css_classes(["title-1"])
        .build();
    title_box.append(&name);
    if let Some(dev) = &app.developer {
        let dev_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        dev_box.append(&gtk::Label::builder().label(dev).xalign(0.0).css_classes(["dim-label"]).build());
        if app.verified {
            let v = gtk::Image::from_icon_name("object-select-symbolic");
            v.add_css_class("accent");
            v.set_tooltip_text(Some("Verified developer"));
            dev_box.append(&v);
        }
        title_box.append(&dev_box);
    }
    if !app.summary.is_empty() {
        title_box.append(&gtk::Label::builder().label(&app.summary).xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    }

    // Actions
    let install = gtk::Button::builder().label("Install").css_classes(["suggested-action", "pill"]).build();
    let open_btn = gtk::Button::builder().label("Open").css_classes(["pill"]).build();
    let update = gtk::Button::builder().label("Update").css_classes(["suggested-action", "pill"]).build();
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Uninstall")
        .css_classes(["destructive-action", "circular"])
        .valign(gtk::Align::Center)
        .build();
    let progress = gtk::ProgressBar::builder().show_text(false).valign(gtk::Align::Center).hexpand(true).build();
    progress.set_pulse_step(0.1);
    let progress_label = gtk::Label::builder().xalign(0.0).css_classes(["caption", "dim-label"]).ellipsize(gtk::pango::EllipsizeMode::End).build();
    let cancel = gtk::Button::builder().icon_name("process-stop-symbolic").tooltip_text("Cancel").css_classes(["circular", "flat"]).build();
    let progress_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    progress_box.set_size_request(220, -1);
    let pb_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    pb_row.append(&progress);
    pb_row.append(&cancel);
    progress_box.append(&pb_row);
    progress_box.append(&progress_label);
    progress_box.set_visible(false);

    let source_dd = gtk::DropDown::from_strings(&[]);
    source_dd.set_tooltip_text(Some("Installation source"));
    let source_label = gtk::Label::builder().css_classes(["caption", "dim-label"]).build();

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&remove);
    buttons.append(&open_btn);
    buttons.append(&update);
    buttons.append(&install);
    buttons.append(&progress_box);

    let actions = gtk::Box::new(gtk::Orientation::Vertical, 8);
    actions.set_valign(gtk::Align::Center);
    actions.append(&buttons);
    let src_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    src_box.set_halign(gtk::Align::End);
    src_box.append(&source_label);
    src_box.append(&source_dd);
    actions.append(&src_box);

    header.append(&title_box);
    header.append(&actions);
    content.append(&header);

    // Media, description and feedback on the left; facts on the right.
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    let main_col = gtk::Box::new(gtk::Orientation::Vertical, 24);
    main_col.set_hexpand(true);
    let side_col = gtk::Box::new(gtk::Orientation::Vertical, 24);
    side_col.set_width_request(300);
    columns.append(&main_col);
    columns.append(&side_col);
    content.append(&columns);

    // ---- Screenshots -------------------------------------------------------
    if !app.screenshots.is_empty() {
        main_col.append(&screenshots(&app));
    }

    // ---- At a glance ------------------------------------------------------
    let tiles = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
    let (source_tile_w, st_title, st_sub) = context_tile("Source", "package-x-generic-symbolic");
    let (size_tile_w, sz_title, sz_sub) = context_tile("Size", "drive-harddisk-symbolic");
    let (safety_tile_w, sf_title, sf_sub) = context_tile("Safety", "security-high-symbolic");
    let safety_icon = safety_tile_w.first_child().and_downcast::<gtk::Image>().unwrap();
    let age = widgets::age_rating(&app.content_rating);
    let (age_w, age_title, age_sub) = context_tile("Age Rating", "avatar-default-symbolic");
    match age {
        Some(0) | Some(1..=3) => {
            age_title.set_label("All Ages");
            age_sub.set_label("Suitable for everyone");
        }
        Some(a) => {
            age_title.set_label(&format!("Ages {a}+"));
            age_sub.set_label("May contain content unsuitable for children");
        }
        None => {
            age_title.set_label("Unknown");
            age_sub.set_label("No age rating information");
        }
    }
    for w in [&source_tile_w, &size_tile_w, &safety_tile_w, &age_w] {
        tiles.append(&gtk::ListBoxRow::builder().child(w).activatable(false).build());
    }
    let glance = adw::PreferencesGroup::builder().title("At a Glance").build();
    glance.add(&tiles);
    side_col.append(&glance);

    // ---- Description --------------------------------------------------------
    if !app.description.is_empty() {
        main_col.append(&widgets::description_widget(&app.description));
    }

    // ---- Release notes -----------------------------------------------------
    if let Some(rel) = app.releases.first() {
        let group = adw::PreferencesGroup::builder().title("What's New").build();
        let date = if rel.timestamp > 0 { widgets::format_date(rel.timestamp) } else { String::new() };
        let label = if date.is_empty() { format!("Version {}", rel.version) } else { format!("Version {} · {date}", rel.version) };
        let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
        b.add_css_class("card");
        b.add_css_class("padded-card");
        b.append(&gtk::Label::builder().label(label).xalign(0.0).css_classes(["heading"]).build());
        if rel.description.is_empty() {
            b.append(&gtk::Label::builder().label("No details for this release").xalign(0.0).css_classes(["dim-label"]).build());
        } else {
            b.append(&widgets::description_widget(&rel.description));
        }
        group.add(&b);
        if app.releases.len() > 1 {
            let exp = adw::ExpanderRow::builder().title("Version History").build();
            for r in app.releases.iter().skip(1) {
                let sub = if r.timestamp > 0 { widgets::format_date(r.timestamp) } else { String::new() };
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&format!("Version {}", r.version)))
                    .subtitle(glib::markup_escape_text(&sub))
                    .build();
                exp.add_row(&row);
            }
            let lb = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).margin_top(12).build();
            lb.append(&exp);
            group.add(&lb);
        }
        main_col.append(&group);
    }

    // ---- Add-ons -------------------------------------------------------------
    let addons = adw::PreferencesGroup::builder().title("Add-ons").visible(false).build();
    main_col.append(&addons);

    // ---- Permissions (flatpak) ---------------------------------------------
    let permissions = adw::PreferencesGroup::builder()
        .title("Permissions")
        .description("What this app can access on your system")
        .visible(false)
        .build();

    // ---- Links -------------------------------------------------------------
    let links = adw::PreferencesGroup::builder().title("Links").build();
    let link_defs = [
        ("homepage", "Project Website", "web-browser-symbolic"),
        ("donation", "Donate", "emote-love-symbolic"),
        ("help", "Help", "help-browser-symbolic"),
        ("bugtracker", "Report an Issue", "dialog-warning-symbolic"),
        ("translate", "Contribute Translations", "preferences-desktop-locale-symbolic"),
        ("contribute", "Contribute", "system-users-symbolic"),
        ("vcs-browser", "Source Code", "text-x-generic-symbolic"),
        ("contact", "Contact", "mail-unread-symbolic"),
        ("faq", "FAQ", "help-faq-symbolic"),
    ];
    let mut any_link = false;
    for (kind, title, icon_name) in link_defs {
        let Some(url) = app.url(kind) else { continue };
        any_link = true;
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(glib::markup_escape_text(url))
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon_name));
        row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        let url = url.to_string();
        row.connect_activated(move |r| {
            let win = r.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&url).launch(win.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
        });
        links.add(&row);
    }
    if any_link {
        side_col.append(&links);
    }

    // ---- Details -------------------------------------------------------------
    let details = adw::PreferencesGroup::builder().title("Details").build();
    let mk_row = |title: &str| {
        let r = adw::ActionRow::builder().title(title).subtitle_selectable(true).build();
        r.add_css_class("property");
        r
    };
    let version_row = mk_row("Version");
    let origin_row = mk_row("Source");
    let package_row = mk_row("Package");
    let installed_size_row = mk_row("Installed Size");
    let download_size_row = mk_row("Download Size");
    let license_row = mk_row("License");
    license_row.set_subtitle(&glib::markup_escape_text(&license_text(app.license.as_deref())));
    for r in [&version_row, &origin_row, &package_row, &license_row, &installed_size_row, &download_size_row] {
        details.add(r);
    }
    if !app.categories.is_empty() {
        let r = mk_row("Categories");
        r.set_subtitle(&glib::markup_escape_text(&app.categories.join(", ")));
        details.add(&r);
    }
    // Details go above links and permissions in the side column.
    side_col.insert_child_after(&details, Some(&glance));
    side_col.append(&permissions);

    // ---- Reviews ---------------------------------------------------------------
    if app.kind == AppKind::Desktop {
        main_col.append(&reviews_section(&app));
    }

    // ---- More by developer --------------------------------------------------
    if let Some(dev) = &app.developer {
        let others = state.catalog().by_developer(dev, &app.id, 6);
        if !others.is_empty() {
            content.append(&widgets::section_title(&format!("Other Apps by {dev}")));
            content.append(&widgets::tile_flowbox(state, &others, &open));
        }
    }

    let clamp = adw::Clamp::builder().maximum_size(1100).child(&content).build();
    let scrolled = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).build();
    let toolbar = adw::ToolbarView::new();
    let hb = adw::HeaderBar::new();
    toolbar.add_top_bar(&hb);
    toolbar.set_content(Some(&scrolled));

    // Single column on medium widths; also stack the header on narrow ones.
    let bin = adw::BreakpointBin::builder().width_request(360).height_request(294).child(&toolbar).build();
    let medium = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        760.0,
        adw::LengthUnit::Sp,
    ));
    medium.add_setter(&columns, "orientation", Some(&gtk::Orientation::Vertical.to_value()));
    bin.add_breakpoint(medium);
    let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        600.0,
        adw::LengthUnit::Sp,
    ));
    bp.add_setter(&header, "orientation", Some(&gtk::Orientation::Vertical.to_value()));
    bp.add_setter(&columns, "orientation", Some(&gtk::Orientation::Vertical.to_value()));
    bp.add_setter(&actions, "halign", Some(&gtk::Align::Start.to_value()));
    bp.add_setter(&icon, "pixel-size", Some(&96.to_value()));
    bp.add_setter(&icon, "halign", Some(&gtk::Align::Start.to_value()));
    bin.add_breakpoint(bp);

    let page = adw::NavigationPage::builder().title(&app.name).child(&bin).tag(&app.id).build();

    let p = Rc::new(Page {
        state: state.clone(),
        app: app.clone(),
        sources: RefCell::new(Vec::new()),
        selected: RefCell::new(None),
        updating_dd: Cell::new(false),
        info_generation: Cell::new(0),
        w: Widgets {
            install,
            open: open_btn,
            update,
            remove,
            progress_box,
            progress,
            progress_label,
            cancel,
            source_dd,
            source_label,
            version_row,
            installed_size_row,
            download_size_row,
            origin_row,
            package_row,
            size_tile: (sz_title, sz_sub),
            safety_tile: (safety_icon, sf_title, sf_sub),
            source_tile: (st_title, st_sub),
            permissions,
            permission_rows: RefCell::new(Vec::new()),
            addons,
            addon_rows: RefCell::new(Vec::new()),
        },
    });

    p.connect_signals();
    p.rebuild_sources();
    p.build_addons();
    p.refresh_actions();
    p.load_info();

    // Live updates while the page exists.
    let weak_page = page.downgrade();
    let pw = Rc::downgrade(&p);
    state.subscribe(move |change| {
        let (Some(_), Some(p)) = (weak_page.upgrade(), pw.upgrade()) else { return false };
        match change {
            Change::Installed => {
                p.rebuild_sources();
                p.refresh_actions();
                p.refresh_addons();
                p.load_info();
            }
            Change::Jobs | Change::Updates => {
                p.refresh_actions();
                p.refresh_addons();
            }
            Change::Catalog => {}
        }
        true
    });
    // Keep the Rc alive as long as the page lives.
    unsafe { page.set_data("page-state", p) };
    page
}

fn license_text(l: Option<&str>) -> String {
    match l {
        None | Some("") => "Unknown".into(),
        Some(l) if l.contains("LicenseRef-proprietary") => "Proprietary".into(),
        Some(l) => l.to_string(),
    }
}

fn context_tile(title: &str, icon: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.add_css_class("context-row");
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(24);
    img.set_valign(gtk::Align::Center);
    img.add_css_class("context-icon");
    b.append(&img);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["caption-heading", "dim-label"]).build());
    let t = gtk::Label::builder().label("…").xalign(0.0).wrap(true).css_classes(["heading"]).build();
    let s = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["caption", "dim-label"]).build();
    text.append(&t);
    text.append(&s);
    b.append(&text);
    (b, t, s)
}

fn screenshots(app: &App) -> gtk::Widget {
    let carousel = adw::Carousel::builder().allow_scroll_wheel(false).spacing(12).build();
    carousel.set_height_request(380);
    for shot in app.screenshots.iter().take(12) {
        let pic = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        pic.set_alternative_text(shot.caption.as_deref());
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        let overlay = gtk::Overlay::builder().child(&pic).hexpand(true).build();
        overlay.add_overlay(&spinner);
        let url = shot.thumbnail.clone().unwrap_or_else(|| shot.full.clone());
        let sp = spinner.clone();
        images::set_picture(&pic, &url, move |ok| {
            sp.set_visible(false);
            let _ = ok;
        });
        // Clicking opens the full-size image.
        let full = shot.full.clone();
        let click = gtk::GestureClick::new();
        click.connect_released(move |g, _, _, _| {
            let win = g.widget().and_then(|w| w.root()).and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&full).launch(win.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
        });
        pic.add_controller(click);
        pic.set_cursor_from_name(Some("zoom-in"));
        carousel.append(&overlay);
    }
    let dots = adw::CarouselIndicatorDots::builder().carousel(&carousel).build();
    let prev = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .css_classes(["circular", "osd"])
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Start)
        .margin_start(12)
        .build();
    let next = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .css_classes(["circular", "osd"])
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .margin_end(12)
        .build();
    let overlay = gtk::Overlay::builder().child(&carousel).build();
    if app.screenshots.len() > 1 {
        overlay.add_overlay(&prev);
        overlay.add_overlay(&next);
    }
    let c = carousel.clone();
    prev.connect_clicked(move |_| {
        let pos = c.position().round() as u32;
        if pos > 0 {
            c.scroll_to(&c.nth_page(pos - 1), true);
        }
    });
    let c = carousel.clone();
    next.connect_clicked(move |_| {
        let pos = c.position().round() as u32;
        if pos + 1 < c.n_pages() {
            c.scroll_to(&c.nth_page(pos + 1), true);
        }
    });
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    b.add_css_class("card");
    b.add_css_class("screenshots");
    b.append(&overlay);
    if app.screenshots.len() > 1 {
        b.append(&dots);
    }
    b.upcast()
}

fn reviews_section(app: &App) -> gtk::Widget {
    let group = adw::PreferencesGroup::builder().title("Ratings &amp; Reviews").build();
    let summary = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    summary.add_css_class("card");
    summary.add_css_class("padded-card");
    let avg = gtk::Label::builder().label("…").css_classes(["title-1"]).build();
    let stars = gtk::Label::builder().css_classes(["stars"]).xalign(0.0).build();
    let count = gtk::Label::builder().css_classes(["dim-label", "caption"]).xalign(0.0).label("Loading ratings…").build();
    let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    col.set_valign(gtk::Align::Center);
    col.append(&stars);
    col.append(&count);
    summary.append(&avg);
    summary.append(&col);
    group.add(&summary);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
    list.set_margin_top(12);
    group.add(&list);

    let id = app.id.clone();
    let version = app.latest_release().map(|r| r.version.clone());
    let weak_avg = avg.downgrade();
    glib::spawn_future_local(async move {
        let res = background(move || (odrs::ratings(&id), odrs::reviews(&id, version.as_deref()))).await;
        let Some(avg) = weak_avg.upgrade() else { return };
        let Some((ratings, reviews)) = res else { return };
        match ratings.filter(|r| r.total > 0) {
            Some(r) => {
                let a = r.average();
                avg.set_label(&format!("{a:.1}"));
                stars.set_label(&widgets::stars_text(a));
                count.set_label(&format!("{} ratings", r.total));
            }
            None => {
                avg.set_label("–");
                stars.set_label("☆☆☆☆☆");
                count.set_label("No ratings yet");
            }
        }
        for r in reviews.iter().take(8) {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
            card.add_css_class("card");
            card.add_css_class("padded-card");
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            top.append(&gtk::Label::builder().label(widgets::stars_text(r.rating as f64 / 20.0)).css_classes(["stars"]).build());
            top.append(&gtk::Label::builder().label(&r.summary).xalign(0.0).hexpand(true).wrap(true).css_classes(["heading"]).build());
            card.append(&top);
            let mut meta = r.reviewer_name.clone().unwrap_or_else(|| "Anonymous".into());
            if r.date_created > 0.0 {
                meta.push_str(" · ");
                meta.push_str(&widgets::format_date(r.date_created as i64));
            }
            if let Some(v) = &r.version {
                meta.push_str(&format!(" · version {v}"));
            }
            card.append(&gtk::Label::builder().label(meta).xalign(0.0).css_classes(["caption", "dim-label"]).build());
            card.append(&gtk::Label::builder().label(&r.description).xalign(0.0).wrap(true).build());
            list.append(&card);
        }
    });
    group.upcast()
}

impl Page {
    fn connect_signals(self: &Rc<Self>) {
        let w = &self.w;
        let p = Rc::downgrade(self);
        w.install.connect_clicked(move |_| {
            let Some(p) = p.upgrade() else { return };
            if let Some(src) = p.selected.borrow().clone() {
                p.state.install(&p.app, src);
            }
        });
        let p = Rc::downgrade(self);
        w.open.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.state.launch(&p.app);
            }
        });
        let p = Rc::downgrade(self);
        w.update.connect_clicked(move |_| {
            let Some(p) = p.upgrade() else { return };
            let Some(src) = p.selected.borrow().clone() else { return };
            p.update_source(&src);
        });
        let p = Rc::downgrade(self);
        w.remove.connect_clicked(move |btn| {
            let Some(p) = p.upgrade() else { return };
            let Some(src) = p.selected.borrow().clone() else { return };
            confirm_remove(btn.upcast_ref(), &p.state, &p.app, src);
        });
        let p = Rc::downgrade(self);
        w.cancel.connect_clicked(move |_| {
            let Some(p) = p.upgrade() else { return };
            if let Some(j) = p.state.job_for(&p.app.id) {
                p.state.cancel_job(j.id);
            }
        });
        let p = Rc::downgrade(self);
        w.source_dd.connect_selected_notify(move |dd| {
            let Some(p) = p.upgrade() else { return };
            if p.updating_dd.get() {
                return;
            }
            let idx = dd.selected() as usize;
            let src = p.sources.borrow().get(idx).cloned();
            if src.is_some() {
                *p.selected.borrow_mut() = src;
                p.refresh_actions();
                p.load_info();
            }
        });
    }

    fn update_source(&self, src: &Source) {
        match src.kind {
            SourceKind::Flatpak => {
                let list: Vec<_> = self
                    .state
                    .updates
                    .borrow()
                    .flatpak
                    .iter()
                    .filter(|u| Some(u.name.as_str()) == src.flatpak_name())
                    .cloned()
                    .collect();
                self.state.update_flatpaks(Some(list));
            }
            _ => {
                let ids: Vec<String> = self.state.updates.borrow().rpm.iter().filter(|p| p.name() == src.id).map(|p| p.id.clone()).collect();
                self.state.update_rpms_live(ids, Some(self.app.id.clone()));
            }
        }
    }

    fn rebuild_sources(&self) {
        let sources = self.state.sorted_sources(&self.app);
        let inst = self.state.installed.borrow();
        let labels: Vec<String> = sources
            .iter()
            .map(|s| {
                let origin = s.display_origin();
                let mut l = if origin == s.kind.label() { origin } else { format!("{origin} · {}", s.kind.label()) };
                if let Some(ch) = s.channel() {
                    l.push_str(&format!(" · {ch}"));
                }
                if inst.has(s) {
                    l.push_str(" ✓");
                }
                l
            })
            .collect();
        drop(inst);
        // Keep the user's choice; otherwise use installed / highest priority source.
        let current = self.selected.borrow().clone();
        let selected = current
            .filter(|c| sources.contains(c))
            .or_else(|| self.state.preferred_source(&self.app));
        let idx = selected.as_ref().and_then(|s| sources.iter().position(|x| x == s)).unwrap_or(0);
        self.updating_dd.set(true);
        let strs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
        self.w.source_dd.set_model(Some(&gtk::StringList::new(&strs)));
        self.w.source_dd.set_selected(idx as u32);
        self.w.source_dd.set_visible(sources.len() > 1);
        self.w.source_label.set_visible(sources.len() > 1);
        self.w.source_label.set_label("Source");
        self.updating_dd.set(false);
        *self.selected.borrow_mut() = sources.get(idx).cloned();
        *self.sources.borrow_mut() = sources;
    }

    fn refresh_actions(&self) {
        let w = &self.w;
        let job = self.state.job_for(&self.app.id);
        if let Some(j) = job {
            for b in [&w.install, &w.open, &w.update, &w.remove] {
                b.set_visible(false);
            }
            w.progress_box.set_visible(true);
            match j.fraction {
                Some(f) => w.progress.set_fraction(f.clamp(0.0, 1.0)),
                None => w.progress.pulse(),
            }
            let label = if j.running { j.status.clone() } else { "Waiting in queue…".to_string() };
            w.progress_label.set_label(&label);
            w.cancel.set_visible(j.running && j.kind != JobKind::Remove);
            w.source_dd.set_sensitive(false);
            return;
        }
        w.progress_box.set_visible(false);
        w.source_dd.set_sensitive(true);
        let sel = self.selected.borrow().clone();
        let Some(src) = sel else {
            w.install.set_visible(false);
            w.open.set_visible(false);
            w.update.set_visible(false);
            w.remove.set_visible(false);
            return;
        };
        let installed = self.state.installed.borrow().has(&src);
        let loaded = self.state.installed.borrow().loaded;
        let has_update = installed && {
            let u = self.state.updates.borrow();
            match src.kind {
                SourceKind::Flatpak => u.flatpak.iter().any(|f| Some(f.name.as_str()) == src.flatpak_name()),
                _ => u.rpm.iter().any(|p| p.name() == src.id),
            }
        };
        w.install.set_visible(!installed);
        w.install.set_sensitive(loaded);
        w.open.set_visible(installed && self.state.can_launch(&self.app));
        w.update.set_visible(has_update);
        w.remove.set_visible(installed);
        // Only one suggested button at a time.
        if has_update {
            w.open.remove_css_class("suggested-action");
        } else {
            w.open.add_css_class("suggested-action");
        }
    }

    /// Load sizes, versions and permissions for the selected source.
    fn load_info(self: &Rc<Self>) {
        let Some(src) = self.selected.borrow().clone() else { return };
        let generation = self.info_generation.get() + 1;
        self.info_generation.set(generation);
        let w = &self.w;
        let installed = self.state.installed.borrow().has(&src);

        w.source_tile.0.set_label(src.kind.label());
        w.source_tile.1.set_label(&src.display_origin());
        w.origin_row.set_subtitle(&glib::markup_escape_text(&format!("{} ({})", src.display_origin(), src.kind.label())));
        w.package_row.set_subtitle(&glib::markup_escape_text(&src.id));
        let inst_version = self.state.installed_version(&src);
        let inst_size = self.state.installed_size(&src).filter(|s| *s > 0);
        let fallback_version = self.app.latest_release().map(|r| r.version.clone());
        w.version_row.set_subtitle(&glib::markup_escape_text(
            &inst_version.clone().or(fallback_version.clone()).unwrap_or_else(|| "Unknown".into()),
        ));
        w.installed_size_row.set_subtitle(&inst_size.map(format_size).unwrap_or_else(|| "…".into()));
        w.download_size_row.set_visible(!installed);
        w.download_size_row.set_subtitle("…");
        w.size_tile.0.set_label(&inst_size.map(format_size).unwrap_or_else(|| "…".into()));
        w.size_tile.1.set_label(if installed { "Installed size" } else { "Download size" });

        // Safety tile
        match src.kind {
            SourceKind::Flatpak => {
                w.safety_tile.0.set_icon_name(Some("security-medium-symbolic"));
                w.safety_tile.1.set_label("Checking…");
                w.safety_tile.2.set_label("");
            }
            SourceKind::Rpm => {
                w.safety_tile.0.set_icon_name(Some("security-medium-symbolic"));
                w.safety_tile.1.set_label("Not Sandboxed");
                w.safety_tile.2.set_label("Distribution package with full system access");
                w.permissions.set_visible(false);
            }
            SourceKind::Terra => {
                w.safety_tile.0.set_icon_name(Some("security-low-symbolic"));
                w.safety_tile.1.set_label("Not Sandboxed");
                w.safety_tile.2.set_label("Third-party repository with full system access");
                w.permissions.set_visible(false);
            }
        }

        let this = Rc::downgrade(self);
        let src2 = src.clone();
        glib::spawn_future_local(async move {
            let res = background(move || load_source_info(&src2, installed)).await;
            let Some(p) = this.upgrade() else { return };
            if p.info_generation.get() != generation {
                return;
            }
            let Some(info) = res else { return };
            let w = &p.w;
            if let Some(v) = info.version.filter(|_| inst_version.is_none()) {
                w.version_row.set_subtitle(&glib::markup_escape_text(&v));
            }
            let isize = inst_size.or(info.installed_size);
            w.installed_size_row.set_subtitle(&isize.map(format_size).unwrap_or_else(|| "Unknown".into()));
            w.download_size_row.set_subtitle(&info.download_size.map(format_size).unwrap_or_else(|| "Unknown".into()));
            let tile_size = if installed { isize } else { info.download_size.or(isize) };
            w.size_tile.0.set_label(&tile_size.map(format_size).unwrap_or_else(|| "Unknown".into()));
            if let Some(perms) = info.permissions {
                p.show_permissions(&perms);
            } else if src.kind == SourceKind::Flatpak {
                w.safety_tile.1.set_label("Unknown");
            }
        });
    }

    fn show_permissions(&self, perms: &[flatpak::Permission]) {
        let w = &self.w;
        for r in w.permission_rows.borrow_mut().drain(..) {
            w.permissions.remove(&r);
        }
        let max = perms.iter().map(|p| p.level).max().unwrap_or(0);
        let (icon, title, sub) = match max {
            0 => ("security-high-symbolic", "Safe", "Sandboxed with minimal permissions"),
            1 => ("security-medium-symbolic", "Probably Safe", "Sandboxed with some permissions"),
            _ => ("security-low-symbolic", "Potentially Unsafe", "Has broad access to your system"),
        };
        w.safety_tile.0.set_icon_name(Some(icon));
        w.safety_tile.1.set_label(title);
        w.safety_tile.2.set_label(sub);
        let mut rows = w.permission_rows.borrow_mut();
        for perm in perms {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&perm.title))
                .subtitle(glib::markup_escape_text(&perm.subtitle))
                .build();
            let img = gtk::Image::from_icon_name(perm.icon);
            if perm.level >= 2 {
                img.add_css_class("warning");
            }
            row.add_prefix(&img);
            w.permissions.add(&row);
            rows.push(row);
        }
        w.permissions.set_visible(!perms.is_empty());
    }

    fn build_addons(self: &Rc<Self>) {
        let addons = self.state.catalog().addons_for(&self.app.id);
        if addons.is_empty() {
            return;
        }
        self.w.addons.set_visible(true);
        let mut rows = self.w.addon_rows.borrow_mut();
        for a in addons.into_iter().take(40) {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&a.name))
                .subtitle(glib::markup_escape_text(&a.summary))
                .build();
            let btn = gtk::Button::builder().valign(gtk::Align::Center).build();
            row.add_suffix(&btn);
            let p = Rc::downgrade(self);
            let addon = a.clone();
            btn.connect_clicked(move |_| {
                let Some(p) = p.upgrade() else { return };
                if let Some(src) = p.state.installed_source(&addon) {
                    p.state.remove(&addon, src);
                } else if let Some(src) = p.state.preferred_source(&addon) {
                    p.state.install(&addon, src);
                }
            });
            self.w.addons.add(&row);
            rows.push((a, btn));
        }
        drop(rows);
        self.refresh_addons();
    }

    fn refresh_addons(&self) {
        for (a, btn) in self.w.addon_rows.borrow().iter() {
            if let Some(j) = self.state.job_for(&a.id) {
                btn.set_label(if j.running { "Working…" } else { "Queued" });
                btn.set_sensitive(false);
                continue;
            }
            btn.set_sensitive(true);
            if self.state.is_installed(a) {
                btn.set_label("Remove");
                btn.remove_css_class("suggested-action");
            } else {
                btn.set_label("Install");
                btn.add_css_class("suggested-action");
            }
        }
    }
}

#[derive(Default)]
struct SourceInfo {
    version: Option<String>,
    installed_size: Option<u64>,
    download_size: Option<u64>,
    permissions: Option<Vec<flatpak::Permission>>,
}

fn load_source_info(src: &Source, installed: bool) -> SourceInfo {
    let mut info = SourceInfo::default();
    match src.kind {
        SourceKind::Flatpak => {
            if !installed
                && let Ok((dl, inst)) = flatpak::remote_size(&src.origin, &src.id, src.user) {
                    info.download_size = Some(dl);
                    info.installed_size = Some(inst);
                }
            info.permissions = flatpak::permissions(&src.origin, &src.id, src.user, installed).ok();
        }
        _ => {
            let filters = packagekit::filter::ARCH | packagekit::filter::NOT_SOURCE;
            let Ok(pkgs) = packagekit::resolve(std::slice::from_ref(&src.id), filters) else { return info };
            let want_terra = src.kind == SourceKind::Terra;
            let pick = pkgs
                .iter()
                .filter(|p| p.name() == src.id)
                .filter(|p| if installed { p.is_installed() } else { !p.is_installed() })
                .max_by(|a, b| {
                    let ta = crate::backend::terra::is_terra_repo(a.repo()) == want_terra;
                    let tb = crate::backend::terra::is_terra_repo(b.repo()) == want_terra;
                    ta.cmp(&tb).then_with(|| crate::backend::rpm_vercmp(a.version(), b.version()))
                });
            if let Some(p) = pick {
                info.version = Some(p.version().to_string());
                if let Ok(d) = packagekit::get_details(std::slice::from_ref(&p.id))
                    && let Some(d) = d.first() {
                        info.installed_size = Some(d.size).filter(|s| *s > 0);
                        info.download_size = Some(d.download_size).filter(|s| *s > 0);
                    }
            }
        }
    }
    info
}

/// Ask before uninstalling.
pub fn confirm_remove(parent: &gtk::Widget, state: &Rc<State>, app: &Arc<App>, src: Source) {
    let body = match src.kind {
        SourceKind::Flatpak => format!("{} will be removed. Its data in ~/.var/app will be kept.", app.name),
        _ => format!("{} (package “{}”) and packages that depend on it will be removed.", app.name, src.id),
    };
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Uninstall {}?", app.name))
        .body(body)
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Uninstall");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let state = state.clone();
    let app = app.clone();
    dialog.connect_response(None, move |_, resp| {
        if resp == "remove" {
            state.remove(&app, src.clone());
        }
    });
    dialog.present(Some(parent));
}
