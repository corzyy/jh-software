mod appstream;
mod backend;
mod catalog;
mod config;
mod images;
mod model;
mod odrs;
mod state;
mod ui;

use adw::prelude::*;
use config::{APP_ID, APP_NAME};
use gtk::{gio, glib};
use state::{Change, State};
use std::cell::RefCell;
use std::rc::Rc;
use ui::window::Window;

struct AppCtx {
    app: adw::Application,
    state: Rc<State>,
    window: RefCell<Option<Rc<Window>>>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
}

impl AppCtx {
    fn window(self: &Rc<Self>) -> Rc<Window> {
        if let Some(w) = self.window.borrow().as_ref() {
            return w.clone();
        }
        let w = Window::new(&self.app, &self.state);
        if let Some(h) = std::env::var("JH_SHOT_HEIGHT").ok().and_then(|h| h.parse().ok()) {
            let wd = std::env::var("JH_SHOT_WIDTH").ok().and_then(|h| h.parse().ok()).unwrap_or(1100);
            w.win.set_default_size(wd, h);
        }
        let ctx = Rc::downgrade(self);
        w.win.connect_close_request(move |win| {
            let Some(ctx) = ctx.upgrade() else { return glib::Propagation::Proceed };
            let background = ctx.state.config.borrow().run_in_background;
            let busy = !ctx.state.jobs().is_empty();
            if background || busy {
                // Keep running: finish operations / watch for updates.
                win.set_visible(false);
                if busy && !background {
                    ctx.hold_while_busy();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        *self.window.borrow_mut() = Some(w.clone());
        w
    }

    fn present(self: &Rc<Self>) -> Rc<Window> {
        let w = self.window();
        w.win.present();
        w
    }

    fn window_visible(&self) -> bool {
        self.window.borrow().as_ref().map(|w| w.win.is_visible() && w.win.is_active()).unwrap_or(false)
    }

    /// Quit once all jobs are finished if the window was closed meanwhile.
    fn hold_while_busy(self: &Rc<Self>) {
        let guard = self.app.hold();
        let guard = RefCell::new(Some(guard));
        let ctx = Rc::downgrade(self);
        self.state.subscribe(move |c| {
            let Some(ctx) = ctx.upgrade() else { return false };
            if c == Change::Jobs && ctx.state.jobs().is_empty() {
                let hidden = ctx.window.borrow().as_ref().map(|w| !w.win.is_visible()).unwrap_or(true);
                guard.borrow_mut().take();
                if hidden && !ctx.state.config.borrow().run_in_background {
                    ctx.app.quit();
                }
                return false;
            }
            true
        });
    }

    fn update_background_hold(&self) {
        if self.state.config.borrow().run_in_background {
            if self.hold.borrow().is_none() {
                *self.hold.borrow_mut() = Some(self.app.hold());
            }
        } else {
            self.hold.borrow_mut().take();
        }
    }
}

fn setup_actions(ctx: &Rc<AppCtx>) {
    let app = &ctx.app;
    let add = |name: &str, f: Box<dyn Fn(&Rc<AppCtx>)>| {
        let a = gio::SimpleAction::new(name, None);
        let c = Rc::downgrade(ctx);
        a.connect_activate(move |_, _| {
            if let Some(c) = c.upgrade() {
                f(&c);
            }
        });
        app.add_action(&a);
    };
    add("quit", Box::new(|c| {
        if !c.state.jobs().is_empty() {
            c.state.toast("Operations are still running; they will finish in the background");
            if let Some(w) = c.window.borrow().as_ref() {
                w.win.set_visible(false);
            }
            c.hold_while_busy();
            return;
        }
        c.hold.borrow_mut().take();
        c.app.quit();
    }));
    add("refresh", Box::new(|c| {
        c.state.refresh_metadata(true);
        c.state.reload_installed();
        c.state.toast("Refreshing software sources…");
    }));
    add("repos", Box::new(|c| {
        let w = c.present();
        ui::dialogs::repositories(&w.win, &c.state);
    }));
    add("install-file", Box::new(|c| {
        let w = c.present();
        ui::dialogs::choose_file(w.win.upcast_ref(), &c.state, &["*.rpm", "*.flatpakref", "*.flatpakrepo"], "Software Packages");
    }));
    add("preferences", Box::new(|c| {
        let w = c.present();
        ui::dialogs::preferences(&w.win, &c.state);
    }));
    add("about", Box::new(|c| {
        let w = c.present();
        ui::dialogs::about(&w.win);
    }));
    add("show-updates", Box::new(|c| {
        let w = c.present();
        w.show_updates();
    }));
    add("offline-update", Box::new(|c| c.state.prepare_offline_update(false)));
    add("live-update", Box::new(|c| {
        let ids: Vec<String> = c.state.updates.borrow().rpm.iter().map(|p| p.id.clone()).collect();
        c.state.update_rpms_live(ids, None);
    }));
    add("background-changed", Box::new(|c| c.update_background_hold()));

    let details = gio::SimpleAction::new("details", Some(glib::VariantTy::STRING));
    let c = Rc::downgrade(ctx);
    details.connect_activate(move |_, v| {
        if let (Some(c), Some(id)) = (c.upgrade(), v.and_then(|v| v.get::<String>())) {
            let w = c.present();
            w.show_app_id(&id);
        }
    });
    app.add_action(&details);

    app.set_accels_for_action("app.quit", &["<Primary>q"]);
    app.set_accels_for_action("app.refresh", &["<Primary>r", "F5"]);
    app.set_accels_for_action("app.preferences", &["<Primary>comma"]);
    app.set_accels_for_action("win.search", &["<Primary>f"]);
    app.set_accels_for_action("win.back", &["<Alt>Left", "Escape"]);
    app.set_accels_for_action("win.show-help-overlay", &["<Primary>question"]);
    app.set_accels_for_action("win.view-1", &["<Alt>1"]);
    app.set_accels_for_action("win.view-2", &["<Alt>2"]);
    app.set_accels_for_action("win.view-3", &["<Alt>3"]);
}

fn handle_command_line(ctx: &Rc<AppCtx>, cmd: &gio::ApplicationCommandLine) -> glib::ExitCode {
    let args: Vec<String> = cmd.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let mut background = false;
    let mut action: Option<Box<dyn FnOnce(&Rc<Window>)>> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--background" | "--gapplication-service" => background = true,
            "--updates" | "--mode=updates" => action = Some(Box::new(|w| w.show_updates())),
            "--details" | "--search" if i + 1 < args.len() => {
                let v = args[i + 1].clone();
                action = Some(if a == "--details" { Box::new(move |w| w.show_app_id(&v)) } else { Box::new(move |w| w.search(&v)) });
                i += 1;
            }
            "-h" | "--help" => {
                cmd.print_literal(&"Usage: jh-software [OPTIONS] [FILE|appstream://ID]\n\n  --background       Run hidden and check for updates\n  --updates          Show the updates page\n  --details ID       Show an app\n  --search TERM      Search\n  --version          Print the version\n\nFILE may be an .rpm, .flatpakref or .flatpakrepo file.\n".to_string());
                return glib::ExitCode::SUCCESS;
            }
            "--version" => {
                cmd.print_literal(&format!("{APP_NAME} {}\n", config::VERSION));
                return glib::ExitCode::SUCCESS;
            }
            s if s.starts_with("appstream:") => {
                let id = s.trim_start_matches("appstream:").trim_start_matches("//").trim_end_matches('/').to_string();
                action = Some(Box::new(move |w| w.show_app_id(&id)));
            }
            s if !s.starts_with('-') => {
                let f = cmd.create_file_for_arg(s);
                if let Some(p) = f.path() {
                    let p = p.to_string_lossy().into_owned();
                    action = Some(Box::new(move |w| w.open_file(&p)));
                }
            }
            _ => {}
        }
        i += 1;
    }

    if background && action.is_none() {
        // Start (or stay) hidden; the hold keeps the process alive.
        *ctx.hold.borrow_mut() = Some(ctx.app.hold());
        return glib::ExitCode::SUCCESS;
    }
    let w = ctx.present();
    if let Some(f) = action {
        f(&w);
    }
    debug_screenshot(&w);
    glib::ExitCode::SUCCESS
}

/// Developer aid: `JH_SHOT=/tmp/x.png [JH_SHOT_DELAY=5] jh-software` renders the
/// window to a PNG after a delay and quits.
fn debug_screenshot(w: &Rc<Window>) {
    let Ok(path) = std::env::var("JH_SHOT") else { return };
    let delay = std::env::var("JH_SHOT_DELAY").ok().and_then(|d| d.parse().ok()).unwrap_or(5);
    let win = w.win.clone();
    glib::timeout_add_seconds_local_once(delay, move || {
        let paintable = gtk::WidgetPaintable::new(Some(&win));
        let (wd, ht) = (win.width() as f64, win.height() as f64);
        let snap = gtk::Snapshot::new();
        paintable.snapshot(&snap, wd, ht);
        if let (Some(node), Some(renderer)) = (snap.to_node(), win.native().and_then(|n| n.renderer())) {
            let tex = renderer.render_texture(&node, None);
            let _ = tex.save_to_png(&path);
        }
        if let Some(app) = win.application() {
            app.quit();
        }
    });
}

/// `jh-software --self-test`: exercise every read-only backend path and print diagnostics.
fn self_test() -> glib::ExitCode {
    use backend::{flatpak, packagekit, terra};
    let t = std::time::Instant::now();
    let cat = catalog::load_cached().unwrap_or_else(catalog::build);
    println!("catalog: {} entries ({:?})", cat.len(), t.elapsed());
    let terra_apps = cat.apps.iter().filter(|a| a.kind == model::AppKind::Desktop && a.sources.iter().any(|s| s.kind == model::SourceKind::Terra)).count();
    println!("apps available from Terra: {terra_apps}");
    for id in std::env::args().skip_while(|a| a != "--self-test").skip(1) {
        match cat.get(&id).or_else(|| cat.by_package(&id)) {
            Some(a) => println!("{id}: {}", a.sources.iter().map(|s| format!("{}:{}", s.kind.label(), s.id)).collect::<Vec<_>>().join(", ")),
            None => println!("{id}: not found"),
        }
    }
    let t = std::time::Instant::now();
    println!("installed rpms: {} ({:?})", backend::rpm::installed().len(), t.elapsed());
    let fl = flatpak::list_installed();
    println!("installed flatpaks: {} ({} apps)", fl.len(), fl.iter().filter(|f| f.is_app).count());
    for r in flatpak::list_remotes() {
        println!("flatpak remote: {} ({}) disabled={} user={}", r.name, r.url, r.disabled, r.user);
    }
    println!("terra packages: {}", terra::list().len());
    match packagekit::find_available("zed", true) {
        Ok(id) => println!("packagekit resolve zed (terra): {id}"),
        Err(e) => println!("packagekit resolve zed: ERROR {e:#}"),
    }
    match packagekit::repo_list() {
        Ok(r) => println!("rpm repos: {} ({} enabled)", r.len(), r.iter().filter(|r| r.enabled).count()),
        Err(e) => println!("rpm repos: ERROR {e:#}"),
    }
    match packagekit::get_updates() {
        Ok(u) => println!("rpm updates: {}", u.len()),
        Err(e) => println!("rpm updates: ERROR {e:#}"),
    }
    match flatpak::list_updates() {
        Ok(u) => println!("flatpak updates: {}", u.len()),
        Err(e) => println!("flatpak updates: ERROR {e:#}"),
    }
    println!("offline update pending: {}", packagekit::offline_update_triggered());
    if let Some(a) = cat.get("org.gnome.Maps")
        && let Some(s) = a.sources.iter().find(|s| s.kind == model::SourceKind::Flatpak) {
            match flatpak::permissions(&s.origin, &s.id, s.user, false) {
                Ok(p) => println!("flatpak permissions {}: {}", a.id, p.iter().map(|p| p.title.as_str()).collect::<Vec<_>>().join(", ")),
                Err(e) => println!("flatpak permissions: ERROR {e}"),
            }
            match flatpak::remote_size(&s.origin, &s.id, s.user) {
                Ok((d, i)) => println!("flatpak size {}: download {} installed {}", a.id, model::format_size(d), model::format_size(i)),
                Err(e) => println!("flatpak size: ERROR {e}"),
            }
        }
    match odrs::ratings("org.gnome.Maps") {
        Some(r) => println!("odrs ratings org.gnome.Maps: {:.1} ({} ratings)", r.average(), r.total),
        None => println!("odrs ratings: unavailable"),
    }
    glib::ExitCode::SUCCESS
}

fn main() -> glib::ExitCode {
    if std::env::args().any(|a| a == "--self-test") {
        return self_test();
    }
    glib::set_application_name(APP_NAME);
    glib::set_prgname(Some("jh-software"));

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let ctx_cell: Rc<RefCell<Option<Rc<AppCtx>>>> = Rc::new(RefCell::new(None));

    let cc = ctx_cell.clone();
    app.connect_startup(move |app| {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("ui/style.css"));
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            // Allow running from the build tree with the bundled icon.
            let theme = gtk::IconTheme::for_display(&display);
            theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
        }
        gtk::Window::set_default_icon_name(APP_ID);

        let state = State::new();
        let ctx = Rc::new(AppCtx { app: app.clone(), state: state.clone(), window: RefCell::new(None), hold: RefCell::new(None) });
        setup_actions(&ctx);

        // Notifications when the window is not in focus.
        let c = Rc::downgrade(&ctx);
        *state.notify.borrow_mut() = Some(Box::new(move |title: &str, body: &str| {
            let Some(c) = c.upgrade() else { return };
            if c.window_visible() {
                return;
            }
            let n = gio::Notification::new(title);
            n.set_body(Some(body));
            n.set_icon(&gio::ThemedIcon::new(APP_ID));
            n.set_default_action("app.show-updates");
            c.app.send_notification(Some("jh-software"), &n);
        }));

        state.load_catalog();
        state.reload_installed();
        state.check_updates();
        images::prune_disk_cache();
        ctx.update_background_hold();

        // Periodic update checks.
        let st = Rc::downgrade(&state);
        glib::timeout_add_seconds_local(30 * 60, move || {
            let Some(s) = st.upgrade() else { return glib::ControlFlow::Break };
            let (auto, last) = {
                let c = s.config.borrow();
                (c.auto_check_updates, c.last_update_check)
            };
            if auto && config::now().saturating_sub(last) > 6 * 3600 {
                s.refresh_metadata(false);
            }
            glib::ControlFlow::Continue
        });

        *cc.borrow_mut() = Some(ctx);
    });

    let cc = ctx_cell.clone();
    app.connect_activate(move |_| {
        if let Some(ctx) = cc.borrow().as_ref() {
            ctx.present();
        }
    });

    let cc = ctx_cell.clone();
    app.connect_command_line(move |_, cmd| {
        let ctx = cc.borrow().clone();
        match ctx {
            Some(ctx) => handle_command_line(&ctx, cmd),
            None => glib::ExitCode::FAILURE,
        }
    });

    app.run()
}
