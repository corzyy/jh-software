//! Application state shared by all views, plus the serialized job queue that
//! runs install / remove / update transactions on a worker thread.

use crate::backend::flatpak::{self, FlatpakUpdate, InstalledFlatpak};
use crate::backend::packagekit::{self, Package};
use crate::backend::rpm::{self, InstalledRpm};
use crate::backend::Progress;
use crate::catalog::{self, Catalog};
use crate::config::{self, Config};
use crate::model::*;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Catalog,
    Installed,
    Updates,
    Jobs,
}

#[derive(Debug, Clone)]
pub struct JobInfo {
    pub id: u64,
    pub app_id: Option<String>,
    pub title: String,
    pub fraction: Option<f64>,
    pub status: String,
    pub running: bool,
    pub kind: JobKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Install,
    Remove,
    Update,
    Other,
}

#[derive(Default)]
pub struct Installed {
    pub loaded: bool,
    pub rpms: HashMap<String, InstalledRpm>,
    pub flatpaks: Vec<InstalledFlatpak>,
}

impl Installed {
    pub fn flatpak_for(&self, src: &Source) -> Option<&InstalledFlatpak> {
        let name = src.flatpak_name()?;
        self.flatpaks.iter().find(|f| f.name == name && f.user == src.user && f.is_app == src.id.starts_with("app/"))
            .or_else(|| self.flatpaks.iter().find(|f| f.name == name && f.is_app == src.id.starts_with("app/")))
    }

    pub fn has(&self, src: &Source) -> bool {
        match src.kind {
            SourceKind::Flatpak => self.flatpak_for(src).is_some(),
            // The same package name can exist in Fedora and Terra; the vendor tells them apart.
            SourceKind::Terra => self.rpms.get(&src.id).is_some_and(|r| r.is_terra()),
            SourceKind::Rpm => self.rpms.get(&src.id).is_some_and(|r| !r.is_terra()),
        }
    }
}

#[derive(Default)]
pub struct Updates {
    pub rpm: Vec<Package>,
    pub flatpak: Vec<FlatpakUpdate>,
    pub checking: bool,
    pub checked: u64,
    pub error: Option<String>,
    pub offline_pending: bool,
}

impl Updates {
    pub fn count(&self) -> usize {
        self.rpm.len() + self.flatpak.len()
    }
}

enum Event {
    Progress { id: u64, fraction: Option<f64>, status: String },
    Done { id: u64, result: Result<String, String> },
}

struct WorkerJob {
    id: u64,
    work: Box<dyn FnOnce(Arc<dyn Progress>) -> anyhow::Result<String> + Send>,
    cancel: Arc<AtomicBool>,
}

struct JobProgress {
    id: u64,
    tx: async_channel::Sender<Event>,
    cancel: Arc<AtomicBool>,
    last: std::sync::Mutex<(Option<f64>, String, std::time::Instant)>,
}

impl Progress for JobProgress {
    fn update(&self, fraction: Option<f64>, status: &str) {
        // Throttle to ~20 updates per second unless the status text changes.
        let mut last = self.last.lock().unwrap();
        let now = std::time::Instant::now();
        if last.1 == status && now.duration_since(last.2).as_millis() < 50 && fraction != Some(1.0) {
            return;
        }
        *last = (fraction, status.to_string(), now);
        let _ = self.tx.try_send(Event::Progress { id: self.id, fraction, status: status.to_string() });
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

type Listener = Box<dyn Fn(Change) -> bool>;
type DoneCallback = Box<dyn FnOnce(&Rc<State>, Result<String, String>)>;

pub struct State {
    pub catalog: RefCell<Arc<Catalog>>,
    pub catalog_ready: Cell<bool>,
    pub installed: RefCell<Installed>,
    pub updates: RefCell<Updates>,
    pub config: RefCell<Config>,
    pub refreshing: Cell<bool>,
    jobs: RefCell<Vec<JobInfo>>,
    cancels: RefCell<HashMap<u64, Arc<AtomicBool>>>,
    callbacks: RefCell<HashMap<u64, DoneCallback>>,
    next_job: Cell<u64>,
    job_tx: async_channel::Sender<WorkerJob>,
    listeners: RefCell<Vec<Listener>>,
    pub toast: RefCell<Option<Box<dyn Fn(&str)>>>,
    pub notify: RefCell<Option<Box<dyn Fn(&str, &str)>>>,
}

/// Run blocking work on a thread pool and await the result on the main loop.
pub async fn background<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    gio::spawn_blocking(f).await.ok()
}

impl State {
    pub fn new() -> Rc<Self> {
        let (job_tx, job_rx) = async_channel::unbounded::<WorkerJob>();
        let (ev_tx, ev_rx) = async_channel::unbounded::<Event>();

        std::thread::Builder::new()
            .name("jobs".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv_blocking() {
                    let progress: Arc<dyn Progress> = Arc::new(JobProgress {
                        id: job.id,
                        tx: ev_tx.clone(),
                        cancel: job.cancel.clone(),
                        last: std::sync::Mutex::new((None, String::new(), std::time::Instant::now())),
                    });
                    let _ = ev_tx.send_blocking(Event::Progress { id: job.id, fraction: None, status: "Starting…".into() });
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.work)(progress)))
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Internal error while running the operation")));
                    let result = res.map_err(|e| format!("{e:#}"));
                    let _ = ev_tx.send_blocking(Event::Done { id: job.id, result });
                }
            })
            .expect("spawn job worker");

        let state = Rc::new(State {
            catalog: RefCell::new(Arc::new(Catalog::empty())),
            catalog_ready: Cell::new(false),
            installed: RefCell::new(Installed::default()),
            updates: RefCell::new(Updates::default()),
            config: RefCell::new(Config::load()),
            refreshing: Cell::new(false),
            jobs: RefCell::new(Vec::new()),
            cancels: RefCell::new(HashMap::new()),
            callbacks: RefCell::new(HashMap::new()),
            next_job: Cell::new(1),
            job_tx,
            listeners: RefCell::new(Vec::new()),
            toast: RefCell::new(None),
            notify: RefCell::new(None),
        });

        let weak = Rc::downgrade(&state);
        glib::spawn_future_local(async move {
            while let Ok(ev) = ev_rx.recv().await {
                let Some(state) = weak.upgrade() else { break };
                state.handle_event(ev);
            }
        });
        state
    }

    // ----- listeners -------------------------------------------------------

    /// Register a listener. It is dropped when it returns `false`.
    pub fn subscribe(&self, f: impl Fn(Change) -> bool + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    pub fn emit(&self, change: Change) {
        // Listeners may subscribe new listeners while running, so take the list.
        let current = std::mem::take(&mut *self.listeners.borrow_mut());
        let mut keep = Vec::with_capacity(current.len());
        for l in current {
            if l(change) {
                keep.push(l);
            }
        }
        let mut list = self.listeners.borrow_mut();
        keep.append(&mut list);
        *list = keep;
    }

    pub fn toast(&self, msg: &str) {
        if let Some(t) = self.toast.borrow().as_ref() {
            t(msg);
        }
    }

    pub fn catalog(&self) -> Arc<Catalog> {
        self.catalog.borrow().clone()
    }

    // ----- loading ---------------------------------------------------------

    pub fn load_catalog(self: &Rc<Self>) {
        let state = self.clone();
        glib::spawn_future_local(async move {
            let cached = background(catalog::load_cached).await.flatten();
            let had_cache = cached.is_some();
            if let Some(c) = cached {
                state.set_catalog(c);
            }
            let stale = if had_cache { background(catalog::is_stale).await.unwrap_or(true) } else { true };
            if stale
                && let Some(c) = background(catalog::build).await {
                    state.set_catalog(c);
                }
            state.maybe_refresh_metadata();
        });
    }

    fn set_catalog(&self, c: Catalog) {
        *self.catalog.borrow_mut() = Arc::new(c);
        self.catalog_ready.set(true);
        self.emit(Change::Catalog);
    }

    pub fn reload_installed(self: &Rc<Self>) {
        let state = self.clone();
        glib::spawn_future_local(async move {
            if let Some((rpms, flatpaks)) = background(|| (rpm::installed(), flatpak::list_installed())).await {
                {
                    let mut i = state.installed.borrow_mut();
                    i.rpms = rpms;
                    i.flatpaks = flatpaks;
                    i.loaded = true;
                }
                state.emit(Change::Installed);
            }
        });
    }

    /// Refresh repository metadata if it is older than a day.
    fn maybe_refresh_metadata(self: &Rc<Self>) {
        let last = self.config.borrow().last_refresh;
        if config::now().saturating_sub(last) > 86400 {
            self.refresh_metadata(false);
        }
    }

    /// Refresh flatpak AppStream and the dnf cache, then rebuild the catalog if needed.
    pub fn refresh_metadata(self: &Rc<Self>, force: bool) {
        if self.refreshing.replace(true) {
            return;
        }
        self.emit(Change::Updates);
        let state = self.clone();
        glib::spawn_future_local(async move {
            let res = background(move || {
                let a = flatpak::update_appstream(&crate::backend::NoProgress);
                let b = packagekit::refresh_cache(force, &crate::backend::NoProgress);
                (a.err().map(|e| e.to_string()), b.err().map(|e| e.to_string()))
            })
            .await;
            if let Some((fe, pe)) = res {
                if let Some(e) = fe.or(pe) {
                    eprintln!("refresh: {e}");
                } else {
                    let mut c = state.config.borrow_mut();
                    c.last_refresh = config::now();
                    c.save();
                }
            }
            if background(catalog::is_stale).await.unwrap_or(false)
                && let Some(c) = background(catalog::build).await {
                    state.set_catalog(c);
                }
            state.refreshing.set(false);
            state.check_updates();
        });
    }

    pub fn check_updates(self: &Rc<Self>) {
        if self.updates.borrow().checking {
            return;
        }
        self.updates.borrow_mut().checking = true;
        self.emit(Change::Updates);
        let state = self.clone();
        glib::spawn_future_local(async move {
            let res = background(|| {
                let rpm = packagekit::get_updates();
                let fp = flatpak::list_updates();
                let offline = packagekit::offline_update_triggered();
                (rpm, fp, offline)
            })
            .await;
            let before = state.updates.borrow().count();
            {
                let mut u = state.updates.borrow_mut();
                u.checking = false;
                u.error = None;
                if let Some((rpm, fp, offline)) = res {
                    let mut errs = Vec::new();
                    match rpm {
                        Ok(r) => u.rpm = r,
                        Err(e) => errs.push(format!("{e:#}")),
                    }
                    match fp {
                        Ok(f) => u.flatpak = f,
                        Err(e) => errs.push(format!("{e:#}")),
                    }
                    u.offline_pending = offline;
                    if !errs.is_empty() {
                        u.error = Some(errs.join("\n"));
                    }
                    u.checked = config::now();
                }
            }
            {
                let mut c = state.config.borrow_mut();
                c.last_update_check = config::now();
                c.save();
            }
            let count = state.updates.borrow().count();
            if count > 0 && count != before
                && let Some(n) = state.notify.borrow().as_ref() {
                    let body = if count == 1 { "1 update is ready to install".to_string() } else { format!("{count} updates are ready to install") };
                    n("Software Updates Available", &body);
                }
            state.emit(Change::Updates);
            if state.config.borrow().auto_update_flatpaks && !state.updates.borrow().flatpak.is_empty() {
                state.update_flatpaks(None);
            }
        });
    }

    // ----- source selection ------------------------------------------------

    /// All sources of an app, best first according to the user's priorities.
    pub fn sorted_sources(&self, app: &App) -> Vec<Source> {
        let cfg = self.config.borrow();
        let mut v = app.sources.clone();
        let user_pref = cfg.flatpak_user;
        v.sort_by_key(|s| {
            let scope = if s.kind == SourceKind::Flatpak && s.user != user_pref { 1 } else { 0 };
            (cfg.priority_of(s.kind), scope)
        });
        v
    }

    pub fn installed_source(&self, app: &App) -> Option<Source> {
        let inst = self.installed.borrow();
        self.sorted_sources(app).into_iter().find(|s| inst.has(s))
    }

    pub fn preferred_source(&self, app: &App) -> Option<Source> {
        self.installed_source(app).or_else(|| self.sorted_sources(app).into_iter().next())
    }

    pub fn is_installed(&self, app: &App) -> bool {
        let inst = self.installed.borrow();
        app.sources.iter().any(|s| inst.has(s))
    }

    pub fn installed_version(&self, src: &Source) -> Option<String> {
        let inst = self.installed.borrow();
        match src.kind {
            SourceKind::Flatpak => inst.flatpak_for(src).and_then(|f| f.version.clone()),
            _ => inst.rpms.get(&src.id).map(|r| r.evr.clone()),
        }
    }

    pub fn installed_size(&self, src: &Source) -> Option<u64> {
        let inst = self.installed.borrow();
        match src.kind {
            SourceKind::Flatpak => inst.flatpak_for(src).map(|f| f.size),
            _ => inst.rpms.get(&src.id).map(|r| r.size),
        }
    }

    pub fn has_update(&self, app: &App) -> bool {
        let u = self.updates.borrow();
        let inst = self.installed.borrow();
        app.sources.iter().filter(|s| inst.has(s)).any(|s| match s.kind {
            SourceKind::Flatpak => u.flatpak.iter().any(|f| Some(f.name.as_str()) == s.flatpak_name()),
            _ => u.rpm.iter().any(|p| p.name() == s.id),
        })
    }

    // ----- jobs ------------------------------------------------------------

    pub fn jobs(&self) -> Vec<JobInfo> {
        self.jobs.borrow().clone()
    }

    /// Identifies the current set of jobs (ignores progress), for cheap change detection.
    pub fn jobs_signature(&self) -> Vec<(u64, bool)> {
        self.jobs.borrow().iter().map(|j| (j.id, j.running)).collect()
    }

    pub fn job_for(&self, app_id: &str) -> Option<JobInfo> {
        self.jobs.borrow().iter().find(|j| j.app_id.as_deref() == Some(app_id)).cloned()
    }

    pub fn cancel_job(&self, id: u64) {
        if let Some(c) = self.cancels.borrow().get(&id) {
            c.store(true, Ordering::Relaxed);
        }
    }

    pub fn submit(
        self: &Rc<Self>,
        title: String,
        app_id: Option<String>,
        kind: JobKind,
        work: impl FnOnce(Arc<dyn Progress>) -> anyhow::Result<String> + Send + 'static,
        on_done: impl FnOnce(&Rc<State>, Result<String, String>) + 'static,
    ) -> u64 {
        let id = self.next_job.get();
        self.next_job.set(id + 1);
        let cancel = Arc::new(AtomicBool::new(false));
        self.jobs.borrow_mut().push(JobInfo {
            id,
            app_id,
            title,
            fraction: None,
            status: "Waiting…".into(),
            running: false,
            kind,
        });
        self.cancels.borrow_mut().insert(id, cancel.clone());
        self.callbacks.borrow_mut().insert(id, Box::new(on_done));
        let _ = self.job_tx.send_blocking(WorkerJob { id, work: Box::new(work), cancel });
        self.emit(Change::Jobs);
        id
    }

    fn handle_event(self: &Rc<Self>, ev: Event) {
        match ev {
            Event::Progress { id, fraction, status } => {
                if let Some(j) = self.jobs.borrow_mut().iter_mut().find(|j| j.id == id) {
                    j.running = true;
                    j.fraction = fraction;
                    j.status = status;
                }
                self.emit(Change::Jobs);
            }
            Event::Done { id, result } => {
                self.jobs.borrow_mut().retain(|j| j.id != id);
                self.cancels.borrow_mut().remove(&id);
                let cb = self.callbacks.borrow_mut().remove(&id);
                if let Some(cb) = cb {
                    cb(self, result);
                }
                self.emit(Change::Jobs);
            }
        }
    }

    fn report(self: &Rc<Self>, result: &Result<String, String>, ok_msg: Option<String>) {
        match result {
            Ok(m) => {
                let msg = if m.is_empty() { ok_msg } else { Some(m.clone()) };
                if let Some(m) = msg {
                    self.toast(&m);
                }
            }
            Err(e) => {
                let first = e.lines().next().unwrap_or(e).to_string();
                self.toast(&first);
                if let Some(n) = self.notify.borrow().as_ref() {
                    n("Operation Failed", e);
                }
            }
        }
    }

    pub fn install(self: &Rc<Self>, app: &App, source: Source) {
        let name = app.name.clone();
        let app_id = app.id.clone();
        let user = source.user;
        let work = move |p: Arc<dyn Progress>| -> anyhow::Result<String> {
            match source.kind {
                SourceKind::Flatpak => flatpak::install(&source.origin, &source.id, user, p)?,
                SourceKind::Terra => packagekit::install_names(std::slice::from_ref(&source.id), true, &*p)?,
                SourceKind::Rpm => packagekit::install_names(std::slice::from_ref(&source.id), false, &*p)?,
            }
            Ok(String::new())
        };
        let msg = format!("{name} is installed");
        self.submit(format!("Installing {}", app.name), Some(app_id), JobKind::Install, work, move |s, r| {
            s.report(&r, Some(msg));
            s.reload_installed();
        });
    }

    pub fn remove(self: &Rc<Self>, app: &App, source: Source) {
        let name = app.name.clone();
        let work = move |p: Arc<dyn Progress>| -> anyhow::Result<String> {
            match source.kind {
                SourceKind::Flatpak => flatpak::uninstall(&source.id, source.user, p)?,
                _ => packagekit::remove_names(std::slice::from_ref(&source.id), &*p)?,
            }
            Ok(String::new())
        };
        let msg = format!("{name} was removed");
        self.submit(format!("Removing {}", app.name), Some(app.id.clone()), JobKind::Remove, work, move |s, r| {
            s.report(&r, Some(msg));
            s.reload_installed();
        });
    }

    /// Update the given flatpak refs, or all available flatpak updates when `None`.
    pub fn update_flatpaks(self: &Rc<Self>, only: Option<Vec<FlatpakUpdate>>) {
        let list = only.unwrap_or_else(|| self.updates.borrow().flatpak.clone());
        if list.is_empty() {
            return;
        }
        let app_id = if list.len() == 1 {
            self.catalog().by_flatpak_name(&list[0].name).map(|a| a.id.clone())
        } else {
            None
        };
        let title = if list.len() == 1 {
            format!("Updating {}", list[0].title.clone().unwrap_or_else(|| list[0].name.clone()))
        } else {
            format!("Updating {} Flatpaks", list.len())
        };
        let work = move |p: Arc<dyn Progress>| -> anyhow::Result<String> {
            for user in [false, true] {
                let refs: Vec<String> = list.iter().filter(|u| u.user == user).map(|u| u.ref_.clone()).collect();
                if !refs.is_empty() {
                    flatpak::update(&refs, user, p.clone())?;
                }
            }
            Ok(String::new())
        };
        self.submit(title, app_id, JobKind::Update, work, |s, r| {
            s.report(&r, Some("Flatpak updates installed".into()));
            s.reload_installed();
            s.check_updates();
        });
    }

    /// Update one RPM (live).
    pub fn update_rpms_live(self: &Rc<Self>, ids: Vec<String>, app_id: Option<String>) {
        if ids.is_empty() {
            return;
        }
        let n = ids.len();
        let work = move |p: Arc<dyn Progress>| -> anyhow::Result<String> {
            packagekit::update_packages(ids, &*p)?;
            Ok(String::new())
        };
        let title = if n == 1 { "Updating package".to_string() } else { format!("Updating {n} packages") };
        self.submit(title, app_id, JobKind::Update, work, |s, r| {
            s.report(&r, Some("System packages updated".into()));
            s.reload_installed();
            s.check_updates();
        });
    }

    /// Download RPM updates and install them on reboot (like GNOME Software).
    pub fn prepare_offline_update(self: &Rc<Self>, then_reboot: bool) {
        let ids: Vec<String> = self.updates.borrow().rpm.iter().map(|p| p.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let work = move |p: Arc<dyn Progress>| -> anyhow::Result<String> {
            packagekit::prepare_offline_update(ids, &*p)?;
            Ok(String::new())
        };
        self.submit("Downloading system updates".into(), None, JobKind::Update, work, move |s, r| {
            let ok = r.is_ok();
            s.report(&r, Some("Updates will be installed on the next restart".into()));
            s.updates.borrow_mut().offline_pending = ok;
            s.emit(Change::Updates);
            if ok && then_reboot
                && let Err(e) = packagekit::request_reboot() {
                    s.toast(&format!("Could not restart: {e}"));
                }
        });
    }

    pub fn cancel_offline_update(self: &Rc<Self>) {
        match packagekit::offline_cancel() {
            Ok(()) => {
                self.updates.borrow_mut().offline_pending = false;
                self.emit(Change::Updates);
            }
            Err(e) => self.toast(&e.to_string()),
        }
    }

    pub fn install_rpm_file(self: &Rc<Self>, path: String) {
        let name = std::path::Path::new(&path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let work = move |p: Arc<dyn Progress>| -> anyhow::Result<String> {
            packagekit::install_files(vec![path], &*p)?;
            Ok(String::new())
        };
        let msg = format!("{name} is installed");
        self.submit(format!("Installing {name}"), Some(format!("file:{name}")), JobKind::Install, work, move |s, r| {
            s.report(&r, Some(msg));
            s.reload_installed();
        });
    }

    pub fn install_flatpakref(self: &Rc<Self>, path: String) {
        let user = self.config.borrow().flatpak_user;
        let name = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let work = move |p: Arc<dyn Progress>| -> anyhow::Result<String> {
            flatpak::install_flatpakref(&path, user, p)?;
            Ok(String::new())
        };
        let msg = format!("{name} is installed");
        self.submit(format!("Installing {name}"), Some(format!("file:{name}")), JobKind::Install, work, move |s, r| {
            s.report(&r, Some(msg));
            s.reload_installed();
            s.load_catalog();
        });
    }

    pub fn add_flatpakrepo(self: &Rc<Self>, path: String) {
        let user = self.config.borrow().flatpak_user;
        let work = move |_p: Arc<dyn Progress>| -> anyhow::Result<String> {
            let name = flatpak::add_flatpakrepo(&path, user)?;
            Ok(format!("Added software source “{name}”"))
        };
        self.submit("Adding software source".into(), None, JobKind::Other, work, |s, r| {
            s.report(&r, None);
            s.load_catalog();
        });
    }

    /// Launch an installed app.
    pub fn launch(&self, app: &App) {
        let Some(src) = self.installed_source(app) else { return };
        let mut candidates = Vec::new();
        if let Some(n) = src.flatpak_name() {
            candidates.push(format!("{n}.desktop"));
        }
        if let Some(l) = &app.launchable {
            candidates.push(l.clone());
        }
        candidates.push(format!("{}.desktop", app.id));
        for c in candidates {
            if let Some(info) = gio::DesktopAppInfo::new(&c) {
                let ctx = gtk::gdk::Display::default().map(|d| d.app_launch_context());
                match info.launch(&[], ctx.as_ref()) {
                    Ok(()) => return,
                    Err(e) => {
                        self.toast(&format!("Could not launch {}: {e}", app.name));
                        return;
                    }
                }
            }
        }
        if src.kind == SourceKind::Flatpak {
            let name = src.flatpak_name().unwrap_or_default().to_string();
            let _ = std::process::Command::new("flatpak").args(["run", &name]).spawn();
        } else {
            self.toast(&format!("{} has no launcher", app.name));
        }
    }

    pub fn can_launch(&self, app: &App) -> bool {
        if !matches!(app.kind, AppKind::Desktop) {
            return false;
        }
        self.installed_source(app).is_some()
    }
}
