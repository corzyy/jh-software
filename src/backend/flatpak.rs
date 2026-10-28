//! Flatpak backend built on libflatpak.

use super::Progress;
use anyhow::{anyhow, Result};
use libflatpak::gio;
use libflatpak::glib;
use libflatpak::prelude::*;
use libflatpak::{Installation, Ref, RefKind, Remote, Transaction, TransactionOperationType};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct InstalledFlatpak {
    /// Full ref, e.g. `app/org.gnome.Maps/x86_64/stable`.
    pub ref_: String,
    pub name: String,
    pub is_app: bool,
    pub origin: String,
    pub user: bool,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub version: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct FlatpakUpdate {
    pub ref_: String,
    pub name: String,
    pub is_app: bool,
    pub origin: String,
    pub user: bool,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteInfo {
    pub name: String,
    pub title: String,
    pub url: String,
    pub disabled: bool,
    pub user: bool,
    pub appstream_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Permission {
    pub icon: &'static str,
    pub title: String,
    pub subtitle: String,
    /// 0 = harmless, 1 = notable, 2 = potentially unsafe
    pub level: u8,
}

fn installation(user: bool) -> Result<Installation> {
    let inst = if user {
        Installation::new_user(gio::Cancellable::NONE)
    } else {
        Installation::new_system(gio::Cancellable::NONE)
    };
    inst.map_err(|e| anyhow!("Cannot open flatpak installation: {e}"))
}

fn installations() -> Vec<(Installation, bool)> {
    let mut v = Vec::new();
    if let Ok(i) = installation(false) {
        v.push((i, false));
    }
    if let Ok(i) = installation(true) {
        v.push((i, true));
    }
    v
}

fn ref_kind_is_app(r: &impl IsA<Ref>) -> bool {
    r.as_ref().kind() == RefKind::App
}

fn gstr(o: Option<glib::GString>) -> Option<String> {
    o.map(|s| s.to_string()).filter(|s| !s.is_empty())
}

pub fn list_installed() -> Vec<InstalledFlatpak> {
    let mut out = Vec::new();
    for (inst, user) in installations() {
        let Ok(refs) = inst.list_installed_refs(gio::Cancellable::NONE) else { continue };
        for r in refs {
            let Some(full) = r.format_ref() else { continue };
            out.push(InstalledFlatpak {
                ref_: full.to_string(),
                name: r.name().map(|s| s.to_string()).unwrap_or_default(),
                is_app: ref_kind_is_app(&r),
                origin: r.origin().map(|s| s.to_string()).unwrap_or_default(),
                user,
                title: gstr(r.appdata_name()),
                summary: gstr(r.appdata_summary()),
                version: gstr(r.appdata_version()),
                size: r.installed_size(),
            });
        }
    }
    out
}

pub fn list_updates() -> Result<Vec<FlatpakUpdate>> {
    let mut out = Vec::new();
    let mut last_err = None;
    for (inst, user) in installations() {
        match inst.list_installed_refs_for_update(gio::Cancellable::NONE) {
            Ok(refs) => {
                for r in refs {
                    let Some(full) = r.format_ref() else { continue };
                    out.push(FlatpakUpdate {
                        ref_: full.to_string(),
                        name: r.name().map(|s| s.to_string()).unwrap_or_default(),
                        is_app: ref_kind_is_app(&r),
                        origin: r.origin().map(|s| s.to_string()).unwrap_or_default(),
                        user,
                        title: gstr(r.appdata_name()),
                    });
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    if out.is_empty()
        && let Some(e) = last_err {
            return Err(anyhow!("Could not check flatpak updates: {e}"));
        }
    Ok(out)
}

pub fn list_remotes() -> Vec<RemoteInfo> {
    let arch = std::env::consts::ARCH;
    let mut out = Vec::new();
    for (inst, user) in installations() {
        let Ok(remotes) = inst.list_remotes(gio::Cancellable::NONE) else { continue };
        for r in remotes {
            let name = r.name().map(|s| s.to_string()).unwrap_or_default();
            out.push(RemoteInfo {
                title: gstr(r.title()).unwrap_or_else(|| name.clone()),
                url: r.url().map(|s| s.to_string()).unwrap_or_default(),
                disabled: r.is_disabled(),
                user,
                appstream_dir: r.appstream_dir(Some(arch)).and_then(|f| f.path()),
                name,
            });
        }
    }
    out
}

pub fn set_remote_enabled(name: &str, user: bool, enabled: bool) -> Result<()> {
    let inst = installation(user)?;
    let remote = inst.remote_by_name(name, gio::Cancellable::NONE)?;
    remote.set_disabled(!enabled);
    inst.modify_remote(&remote, gio::Cancellable::NONE)?;
    Ok(())
}

/// Refresh the AppStream metadata of every enabled remote.
pub fn update_appstream(progress: &dyn Progress) -> Result<()> {
    let mut errors = Vec::new();
    for (inst, _user) in installations() {
        let Ok(remotes) = inst.list_remotes(gio::Cancellable::NONE) else { continue };
        for r in remotes.iter().filter(|r| !r.is_disabled() && !r.is_noenumerate()) {
            let name = r.name().map(|s| s.to_string()).unwrap_or_default();
            progress.update(None, &format!("Refreshing {name}…"));
            if let Err(e) = inst.update_appstream_sync(&name, None, gio::Cancellable::NONE) {
                errors.push(format!("{name}: {e}"));
            }
        }
        let _ = inst.drop_caches(gio::Cancellable::NONE);
    }
    if errors.is_empty() { Ok(()) } else { Err(anyhow!(errors.join("\n"))) }
}

/// Run a flatpak transaction with progress reporting.
fn run_transaction(
    user: bool,
    progress: Arc<dyn Progress>,
    accept_new_remotes: bool,
    setup: impl FnOnce(&Transaction) -> std::result::Result<(), glib::Error>,
) -> Result<()> {
    let ctx = glib::MainContext::new();
    ctx.with_thread_default(|| -> Result<()> {
        let inst = installation(user)?;
        let tx = Transaction::for_installation(&inst, gio::Cancellable::NONE)?;
        tx.add_default_dependency_sources();
        setup(&tx)?;

        let cancellable = gio::Cancellable::new();
        let done_ops = Rc::new(Cell::new(0usize));
        let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        {
            let progress = progress.clone();
            let done_ops = done_ops.clone();
            let cancellable = cancellable.clone();
            tx.connect_new_operation(move |tx, op, prog| {
                let total = tx.operations().len().max(1);
                let index = done_ops.get();
                let verb = match op.operation_type() {
                    TransactionOperationType::Install | TransactionOperationType::InstallBundle => "Installing",
                    TransactionOperationType::Update => "Updating",
                    TransactionOperationType::Uninstall => "Removing",
                    _ => "Processing",
                };
                let what = op
                    .get_ref()
                    .and_then(|r| r.split('/').nth(1).map(String::from))
                    .unwrap_or_default();
                let status = if total > 1 {
                    format!("{verb} {what} ({}/{total})…", index + 1)
                } else {
                    format!("{verb} {what}…")
                };
                progress.update(Some(index as f64 / total as f64), &status);
                prog.set_update_frequency(150);
                let progress = progress.clone();
                let cancellable = cancellable.clone();
                prog.connect_changed(move |p| {
                    let frac = (index as f64 + p.progress().clamp(0, 100) as f64 / 100.0) / total as f64;
                    progress.update(Some(frac), &status);
                    if progress.cancelled() {
                        cancellable.cancel();
                    }
                });
            });
        }
        {
            let done_ops = done_ops.clone();
            tx.connect_operation_done(move |_, _, _, _| done_ops.set(done_ops.get() + 1));
        }
        {
            let error = error.clone();
            tx.connect_operation_error(move |_, op, err, details| {
                let what = op.get_ref().map(|r| r.to_string()).unwrap_or_default();
                let fatal = !details.contains(libflatpak::TransactionErrorDetails::FATAL);
                if fatal {
                    *error.borrow_mut() = Some(format!("{what}: {}", err.message()));
                }
                // Continue only on non-fatal errors.
                !fatal
            });
        }
        tx.connect_add_new_remote(move |_, _, _, _, _| accept_new_remotes);
        tx.connect_ready(|_| true);

        let res = tx.run(Some(&cancellable));
        if let Some(e) = error.borrow_mut().take() {
            return Err(anyhow!(e));
        }
        res.map_err(|e| {
            if e.matches(gio::IOErrorEnum::Cancelled) {
                anyhow!("Operation was cancelled")
            } else {
                anyhow!("{}", e.message())
            }
        })
    })
    .map_err(|e| anyhow!("{e}"))?
}

pub fn install(remote: &str, ref_: &str, user: bool, progress: Arc<dyn Progress>) -> Result<()> {
    let (remote, ref_) = (remote.to_string(), ref_.to_string());
    run_transaction(user, progress, false, move |tx| tx.add_install(&remote, &ref_, &[]))
}

pub fn uninstall(ref_: &str, user: bool, progress: Arc<dyn Progress>) -> Result<()> {
    let ref_ = ref_.to_string();
    run_transaction(user, progress, false, move |tx| tx.add_uninstall(&ref_))
}

pub fn update(refs: &[String], user: bool, progress: Arc<dyn Progress>) -> Result<()> {
    let refs = refs.to_vec();
    run_transaction(user, progress, false, move |tx| {
        for r in &refs {
            tx.add_update(r, &[], None)?;
        }
        Ok(())
    })
}

pub fn install_flatpakref(path: &str, user: bool, progress: Arc<dyn Progress>) -> Result<()> {
    let data = std::fs::read(path)?;
    run_transaction(user, progress, true, move |tx| tx.add_install_flatpakref(&glib::Bytes::from_owned(data)))
}

pub fn add_flatpakrepo(path: &str, user: bool) -> Result<String> {
    let data = std::fs::read(path)?;
    let name = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase().replace(' ', "-"))
        .unwrap_or_else(|| "remote".into());
    let remote = Remote::from_file(&name, &glib::Bytes::from_owned(data))?;
    let inst = installation(user)?;
    inst.add_remote(&remote, true, gio::Cancellable::NONE)?;
    let _ = inst.update_appstream_sync(&name, None, gio::Cancellable::NONE);
    Ok(name)
}

/// Download and installed size of a remote ref.
pub fn remote_size(remote: &str, ref_: &str, user: bool) -> Result<(u64, u64)> {
    let inst = installation(user)?;
    let r = Ref::parse(ref_)?;
    Ok(inst.fetch_remote_size_sync(remote, &r, gio::Cancellable::NONE)?)
}

/// Permissions of an installed ref, or of a remote ref when not installed.
pub fn permissions(remote: &str, ref_: &str, user: bool, installed: bool) -> Result<Vec<Permission>> {
    let inst = installation(user)?;
    let r = Ref::parse(ref_)?;
    let bytes = if installed {
        let name = r.name().map(|s| s.to_string()).unwrap_or_default();
        let ir = inst.installed_ref(
            r.kind(),
            &name,
            r.arch().as_deref(),
            r.branch().as_deref(),
            gio::Cancellable::NONE,
        )?;
        ir.load_metadata(gio::Cancellable::NONE)?
    } else {
        inst.fetch_remote_metadata_sync(remote, &r, gio::Cancellable::NONE)?
    };
    let kf = glib::KeyFile::new();
    kf.load_from_bytes(&bytes, glib::KeyFileFlags::NONE)?;
    Ok(parse_permissions(&kf))
}

fn list(kf: &glib::KeyFile, group: &str, key: &str) -> Vec<String> {
    kf.string_list(group, key)
        .map(|l| l.iter().map(|s| s.to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn parse_permissions(kf: &glib::KeyFile) -> Vec<Permission> {
    let mut p = Vec::new();
    let shared = list(kf, "Context", "shared");
    let sockets = list(kf, "Context", "sockets");
    let devices = list(kf, "Context", "devices");
    let fs = list(kf, "Context", "filesystems");
    let mk = |icon, title: &str, subtitle: &str, level| Permission { icon, title: title.into(), subtitle: subtitle.into(), level };

    if shared.iter().any(|s| s == "network") {
        p.push(mk("network-wireless-symbolic", "Network Access", "Can access the internet", 1));
    } else {
        p.push(mk("network-wireless-disabled-symbolic", "No Network Access", "Cannot access the internet", 0));
    }
    if sockets.iter().any(|s| s == "x11") && !sockets.iter().any(|s| s == "fallback-x11") {
        p.push(mk("dialog-warning-symbolic", "Legacy Windowing System", "Uses X11, which allows reading input of other windows", 2));
    }
    if sockets.iter().any(|s| s == "system-bus") || sockets.iter().any(|s| s == "session-bus") {
        p.push(mk("dialog-warning-symbolic", "Full Message Bus Access", "Can talk to and control other software", 2));
    }
    if sockets.iter().any(|s| s == "ssh-auth") {
        p.push(mk("dialog-password-symbolic", "SSH Key Access", "Can use your SSH keys", 2));
    }
    if devices.iter().any(|d| d == "all") {
        p.push(mk("camera-web-symbolic", "Device Access", "Can access all devices such as webcams and USB storage", 2));
    } else if devices.iter().any(|d| d == "dri") {
        p.push(mk("video-display-symbolic", "GPU Acceleration", "Uses the graphics card", 0));
    }
    if sockets.iter().any(|s| s == "pulseaudio") {
        p.push(mk("audio-speakers-symbolic", "Audio", "Can play sound and possibly use the microphone", 1));
    }
    let mut fs_full = false;
    let mut fs_home = false;
    let mut fs_other = Vec::new();
    for f in &fs {
        let base = f.split(':').next().unwrap_or("");
        match base {
            "host" | "host-os" | "host-etc" | "/" => fs_full = true,
            "home" | "~" => fs_home = true,
            _ => fs_other.push(f.clone()),
        }
    }
    if fs_full {
        p.push(mk("folder-symbolic", "Full File System Access", "Can read and write all your data", 2));
    } else if fs_home {
        p.push(mk("user-home-symbolic", "Home Folder Access", "Can read and write all data in your home folder", 2));
    } else if !fs_other.is_empty() {
        p.push(mk("folder-symbolic", "File System Access", &fs_other.join(", "), 1));
    } else {
        p.push(mk("folder-symbolic", "No File System Access", "Can only use files you explicitly open", 0));
    }
    let talks = kf.keys("Session Bus Policy").map(|k| k.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap_or_default();
    if talks.iter().any(|n| n == "org.freedesktop.Flatpak") {
        p.push(mk("dialog-warning-symbolic", "Can Escape the Sandbox", "Can run commands on the host system", 2));
    }
    if talks.iter().any(|n| n.starts_with("org.freedesktop.secrets")) {
        p.push(mk("dialog-password-symbolic", "Keyring Access", "Can read stored passwords", 1));
    }
    if let Ok(eol) = kf.string("Application", "end-of-life") {
        p.push(mk("dialog-warning-symbolic", "End of Life", eol.as_str(), 2));
    }
    p
}
