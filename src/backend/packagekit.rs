//! PackageKit D-Bus client (RPM installs, removals, updates, repositories).
//!
//! PackageKit handles polkit authentication and talks to dnf5 for us, exactly
//! like GNOME Software does.

use super::Progress;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::sync::OnceLock;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const DEST: &str = "org.freedesktop.PackageKit";
const PATH: &str = "/org/freedesktop/PackageKit";
const IFACE_TX: &str = "org.freedesktop.PackageKit.Transaction";
const IFACE_OFFLINE: &str = "org.freedesktop.PackageKit.Offline";

// Filter enum values (pk-enum.h); the D-Bus API takes a bitfield of 1 << value.
pub mod filter {
    pub const NONE: u64 = 1 << 1;
    pub const INSTALLED: u64 = 1 << 2;
    pub const NOT_INSTALLED: u64 = 1 << 3;
    pub const NOT_DEVELOPMENT: u64 = 1 << 5;
    pub const ARCH: u64 = 1 << 18;
    pub const NOT_SOURCE: u64 = 1 << 21;
}

mod flag {
    pub const ONLY_TRUSTED: u64 = 1 << 1;
    pub const ONLY_DOWNLOAD: u64 = 1 << 3;
}

// Info enum values.
pub mod info {
    pub const INSTALLED: u32 = 1;
    pub const IMPORTANT: u32 = 7;
    pub const SECURITY: u32 = 8;
    pub const BLOCKED: u32 = 9;
    pub const CRITICAL: u32 = 26;
}

const EXIT_SUCCESS: u32 = 1;
const EXIT_CANCELLED: u32 = 3;

#[derive(Debug, Clone)]
pub struct Package {
    pub info: u32,
    /// `name;version;arch;data`
    pub id: String,
    pub summary: String,
}

impl Package {
    fn part(&self, i: usize) -> &str {
        self.id.split(';').nth(i).unwrap_or("")
    }
    pub fn name(&self) -> &str {
        self.part(0)
    }
    pub fn version(&self) -> &str {
        self.part(1)
    }
    pub fn arch(&self) -> &str {
        self.part(2)
    }
    /// Repository id (with the `installed:` prefix stripped).
    pub fn repo(&self) -> &str {
        let d = self.part(3);
        d.strip_prefix("installed:").unwrap_or(d)
    }
    pub fn is_installed(&self) -> bool {
        self.info == info::INSTALLED || self.part(3).starts_with("installed")
    }
    pub fn is_security(&self) -> bool {
        matches!(self.info, info::SECURITY | info::CRITICAL | info::IMPORTANT)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Details {
    pub size: u64,
    pub download_size: u64,
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub id: String,
    pub description: String,
    pub enabled: bool,
}

#[derive(Default)]
struct Collected {
    packages: Vec<Package>,
    details: Vec<Details>,
    repos: Vec<Repo>,
}

fn connection() -> Result<&'static Connection> {
    static CONN: OnceLock<std::result::Result<Connection, String>> = OnceLock::new();
    CONN.get_or_init(|| Connection::system().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| anyhow!("Cannot connect to the system bus: {e}"))
}

fn status_text(status: u32) -> &'static str {
    match status {
        1 => "Waiting in queue…",
        2 | 3 => "Starting…",
        4 | 5 => "Querying…",
        6 => "Removing packages…",
        7 | 20 | 21 | 22 | 25 => "Refreshing software sources…",
        8 => "Downloading…",
        9 => "Installing…",
        10 => "Updating…",
        11 => "Cleaning up…",
        13 => "Resolving dependencies…",
        14 => "Checking signatures…",
        15 => "Testing changes…",
        16 => "Committing changes…",
        17 => "Requesting data…",
        18 => "Finished",
        19 => "Cancelling…",
        27 => "Loading cache…",
        30 => "Waiting for package manager lock…",
        31 => "Waiting for authentication…",
        _ => "Working…",
    }
}

fn hints() -> Vec<String> {
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into());
    vec![
        format!("locale={locale}"),
        "interactive=true".into(),
        "background=false".into(),
        "supports-plural-signals=false".into(),
        "cache-age=3600".into(),
    ]
}

/// Run one PackageKit transaction. `call` invokes the transaction method.
fn run_tx(
    progress: &dyn Progress,
    call: impl FnOnce(&Proxy) -> zbus::Result<()>,
) -> Result<Collected> {
    let conn = connection()?;
    let pk = Proxy::new(conn, DEST, PATH, DEST).context("PackageKit is not available")?;
    let path: OwnedObjectPath = pk.call("CreateTransaction", &()).context("Could not create a PackageKit transaction")?;

    // Subscribe before starting so no signal is lost.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(path.clone())?
        .build();
    let iter = MessageIterator::for_match_rule(rule, conn, Some(1024))?;

    let tx = Proxy::new(conn, DEST, path.clone(), IFACE_TX)?;
    let _ = tx.call_method("SetHints", &(hints(),));
    call(&tx).map_err(map_dbus_error)?;

    let mut out = Collected::default();
    let mut error: Option<String> = None;
    let mut percentage: Option<f64> = None;
    let mut status = 0u32;

    for msg in iter {
        let msg = msg?;
        let header = msg.header();
        let member = header.member().map(|m| m.as_str().to_owned()).unwrap_or_default();
        let body = msg.body();
        match member.as_str() {
            "Package" => {
                if let Ok((info, id, summary)) = body.deserialize::<(u32, String, String)>() {
                    out.packages.push(Package { info, id, summary });
                }
            }
            "Details" => {
                if let Ok(map) = body.deserialize::<HashMap<String, OwnedValue>>() {
                    out.details.push(parse_details(&map));
                }
            }
            "RepoDetail" => {
                if let Ok((id, description, enabled)) = body.deserialize::<(String, String, bool)>() {
                    out.repos.push(Repo { id, description, enabled });
                }
            }
            "ErrorCode" => {
                if let Ok((code, details)) = body.deserialize::<(u32, String)>() {
                    error = Some(error_text(code, &details));
                }
            }
            "PropertiesChanged" => {
                if let Ok((_, changed, _)) = body.deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() {
                    if let Some(p) = changed.get("Percentage").and_then(|v| u32::try_from(v).ok()) {
                        percentage = if p <= 100 { Some(p as f64 / 100.0) } else { None };
                    }
                    if let Some(s) = changed.get("Status").and_then(|v| u32::try_from(v).ok()) {
                        status = s;
                    }
                    progress.update(percentage, status_text(status));
                }
            }
            "ItemProgress" => {}
            "Finished" => {
                let (exit, _runtime) = body.deserialize::<(u32, u32)>().unwrap_or((0, 0));
                if exit == EXIT_SUCCESS {
                    return Ok(out);
                }
                if exit == EXIT_CANCELLED {
                    bail!("Operation was cancelled");
                }
                bail!(error.unwrap_or_else(|| "The package manager reported an error".into()));
            }
            "Destroy" => {
                bail!(error.unwrap_or_else(|| "Transaction ended unexpectedly".into()));
            }
            _ => {}
        }
        if progress.cancelled() {
            let _ = tx.call_method("Cancel", &());
        }
    }
    bail!("Lost connection to PackageKit")
}

fn map_dbus_error(e: zbus::Error) -> anyhow::Error {
    match &e {
        zbus::Error::MethodError(name, msg, _) => {
            let n = name.as_str();
            if n.contains("NotAuthorized") || n.contains("RefusedByPolicy") {
                anyhow!("Authentication was not granted")
            } else {
                anyhow!(msg.clone().unwrap_or_else(|| n.to_string()))
            }
        }
        _ => anyhow!(e),
    }
}

fn parse_details(map: &HashMap<String, OwnedValue>) -> Details {
    let t = |k: &str| map.get(k).and_then(|v| u64::try_from(v).ok()).unwrap_or(0);
    Details {
        size: t("size"),
        download_size: t("download-size"),
    }
}

fn error_text(code: u32, details: &str) -> String {
    let base = match code {
        1 => "Out of memory",
        2 | 15 => "No network connection",
        3 => "Not supported by the package manager",
        5 | 37 => "Software source could not be reached",
        8 => "The package was not found",
        9 => "The package is already installed",
        10 => "The package is not installed",
        12 => "Dependency resolution failed",
        13 => "Conflicting packages",
        20 | 30 => "Package signature verification failed",
        23 => "Not enough free disk space",
        45 => "Transaction was cancelled",
        52 => "Package manager is locked",
        _ => "",
    };
    let details = details.trim();
    match (base.is_empty(), details.is_empty()) {
        (true, true) => format!("Package manager error {code}"),
        (true, false) => details.to_string(),
        (false, true) => base.to_string(),
        (false, false) => format!("{base}: {details}"),
    }
}

// ----- Public API ---------------------------------------------------------

pub fn resolve(names: &[String], filters: u64) -> Result<Vec<Package>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let names = names.to_vec();
    Ok(run_tx(&super::NoProgress, move |tx| {
        tx.call_method("Resolve", &(filters, names)).map(|_| ())
    })?
    .packages)
}

pub fn get_details(ids: &[String]) -> Result<Vec<Details>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = ids.to_vec();
    Ok(run_tx(&super::NoProgress, move |tx| tx.call_method("GetDetails", &(ids,)).map(|_| ()))?.details)
}

pub fn get_updates() -> Result<Vec<Package>> {
    let mut pkgs = run_tx(&super::NoProgress, |tx| tx.call_method("GetUpdates", &(filter::NONE,)).map(|_| ()))?.packages;
    pkgs.retain(|p| p.info != info::BLOCKED);
    pkgs.sort_by(|a, b| a.name().cmp(b.name()));
    pkgs.dedup_by(|a, b| a.id == b.id);
    Ok(pkgs)
}

pub fn refresh_cache(force: bool, progress: &dyn Progress) -> Result<()> {
    run_tx(progress, move |tx| tx.call_method("RefreshCache", &(force,)).map(|_| ()))?;
    Ok(())
}

/// Pick the best package id for `name` from the given repo class.
pub fn find_available(name: &str, terra: bool) -> Result<String> {
    let pkgs = resolve(&[name.to_string()], filter::NOT_INSTALLED | filter::ARCH | filter::NOT_SOURCE)?;
    let arch = std::env::consts::ARCH;
    let mut candidates: Vec<&Package> = pkgs
        .iter()
        .filter(|p| p.name() == name && !p.is_installed())
        .filter(|p| p.arch() == arch || p.arch() == "noarch")
        .collect();
    let matching: Vec<&Package> = candidates
        .iter()
        .copied()
        .filter(|p| super::terra::is_terra_repo(p.repo()) == terra)
        .collect();
    if !matching.is_empty() {
        candidates = matching;
    }
    candidates
        .into_iter()
        .max_by(|a, b| super::rpm_vercmp(a.version(), b.version()))
        .map(|p| p.id.clone())
        .ok_or_else(|| anyhow!("Package “{name}” is not available from any enabled repository"))
}

pub fn install_packages(ids: Vec<String>, progress: &dyn Progress) -> Result<()> {
    run_tx(progress, move |tx| tx.call_method("InstallPackages", &(flag::ONLY_TRUSTED, ids)).map(|_| ()))?;
    Ok(())
}

pub fn install_names(names: &[String], terra: bool, progress: &dyn Progress) -> Result<()> {
    progress.update(None, "Resolving…");
    let mut ids = Vec::new();
    for n in names {
        ids.push(find_available(n, terra)?);
    }
    install_packages(ids, progress)
}

pub fn remove_names(names: &[String], progress: &dyn Progress) -> Result<()> {
    progress.update(None, "Resolving…");
    let pkgs = resolve(names, filter::INSTALLED)?;
    let ids: Vec<String> = pkgs.into_iter().filter(|p| names.iter().any(|n| n == p.name())).map(|p| p.id).collect();
    if ids.is_empty() {
        bail!("The package is not installed");
    }
    run_tx(progress, move |tx| tx.call_method("RemovePackages", &(0u64, ids, true, false)).map(|_| ()))?;
    Ok(())
}

pub fn install_files(files: Vec<String>, progress: &dyn Progress) -> Result<()> {
    run_tx(progress, move |tx| tx.call_method("InstallFiles", &(0u64, files)).map(|_| ()))?;
    Ok(())
}

/// Apply updates immediately.
pub fn update_packages(ids: Vec<String>, progress: &dyn Progress) -> Result<()> {
    run_tx(progress, move |tx| tx.call_method("UpdatePackages", &(flag::ONLY_TRUSTED, ids)).map(|_| ()))?;
    Ok(())
}

/// Download updates and schedule them to be installed on the next reboot.
pub fn prepare_offline_update(ids: Vec<String>, progress: &dyn Progress) -> Result<()> {
    run_tx(progress, move |tx| {
        tx.call_method("UpdatePackages", &(flag::ONLY_TRUSTED | flag::ONLY_DOWNLOAD, ids)).map(|_| ())
    })?;
    offline_trigger()
}

pub fn offline_trigger() -> Result<()> {
    let conn = connection()?;
    let p = Proxy::new(conn, DEST, PATH, IFACE_OFFLINE)?;
    p.call_method("Trigger", &("reboot",)).map_err(map_dbus_error)?;
    Ok(())
}

pub fn offline_cancel() -> Result<()> {
    let conn = connection()?;
    let p = Proxy::new(conn, DEST, PATH, IFACE_OFFLINE)?;
    p.call_method("Cancel", &()).map_err(map_dbus_error)?;
    Ok(())
}

/// True if an offline update is downloaded and scheduled for the next boot.
pub fn offline_update_triggered() -> bool {
    let Ok(conn) = connection() else { return false };
    let Ok(p) = Proxy::new(conn, DEST, PATH, IFACE_OFFLINE) else { return false };
    p.get_property::<bool>("UpdateTriggered").unwrap_or(false)
}

pub fn repo_list() -> Result<Vec<Repo>> {
    let mut repos = run_tx(&super::NoProgress, |tx| {
        tx.call_method("GetRepoList", &(filter::NOT_DEVELOPMENT | filter::NOT_SOURCE,)).map(|_| ())
    })?
    .repos;
    repos.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(repos)
}

pub fn repo_enable(id: String, enabled: bool) -> Result<()> {
    run_tx(&super::NoProgress, move |tx| tx.call_method("RepoEnable", &(id, enabled)).map(|_| ()))?;
    Ok(())
}

/// Ask the session to reboot (shows the GNOME confirmation dialog), falling back to logind.
pub fn request_reboot() -> Result<()> {
    if let Ok(session) = Connection::session()
        && let Ok(p) = Proxy::new(&session, "org.gnome.SessionManager", "/org/gnome/SessionManager", "org.gnome.SessionManager")
            && p.call_method("Reboot", &()).is_ok() {
                return Ok(());
            }
    let conn = connection()?;
    let p = Proxy::new(conn, "org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager")?;
    p.call_method("Reboot", &(true,)).map_err(map_dbus_error)?;
    Ok(())
}
