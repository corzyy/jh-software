//! Terra repository listing.
//!
//! Terra does not ship AppStream metadata, so the package list is read from
//! the dnf5 metadata cache and matched against AppStream components later.

use std::path::PathBuf;
use std::process::Command;

pub const REPO_ID: &str = "terra";

#[derive(Debug, Clone)]
pub struct TerraPackage {
    pub name: String,
    pub summary: String,
    pub url: String,
    pub license: String,
}

/// The repomd.xml files of the Terra repos in the dnf5 cache, used to detect changes.
pub fn metadata_files() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/var/cache/libdnf5") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_terra_repo(name.rsplit_once('-').map(|(a, _)| a).unwrap_or(&name)) {
                let p = e.path().join("repodata/repomd.xml");
                if p.exists() {
                    v.push(p);
                }
            }
        }
    }
    v.sort();
    v
}

pub fn is_terra_repo(id: &str) -> bool {
    id == REPO_ID || (id.starts_with("terra") && !id.contains("source") && !id.contains("debug"))
}

/// List the newest available package of every name in the Terra repos.
pub fn list() -> Vec<TerraPackage> {
    const SEP: &str = "\u{1f}";
    let qf = format!("%{{name}}{SEP}%{{summary}}{SEP}%{{url}}{SEP}%{{license}}\n");
    let arch = format!("{},noarch", std::env::consts::ARCH);
    let out = Command::new("dnf5")
        .args(["repoquery", "-C", "-q", "--repo", "terra*", "--latest-limit", "1", "--arch", &arch, "--qf", &qf])
        .env("LC_ALL", "C.UTF-8")
        .output();
    let Ok(out) = out else { return Vec::new() };
    let mut v: Vec<TerraPackage> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split(SEP);
            let name = f.next()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(TerraPackage {
                name,
                summary: f.next().unwrap_or("").to_string(),
                url: f.next().unwrap_or("").to_string(),
                license: f.next().unwrap_or("").to_string(),
            })
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v.dedup_by(|a, b| a.name == b.name);
    v
}

/// Packages that are never interesting as stand-alone software.
pub fn is_noise(name: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "-devel", "-libs", "-lib", "-debuginfo", "-debugsource", "-doc", "-docs", "-static", "-headers", "-common",
        "-data", "-filesystem", "-srpm-macros", "-rpm-macros", "-macros", "-tests", "-examples", "-langpack",
    ];
    const PREFIXES: &[&str] = &[
        "golang-", "rust-", "python3-", "perl-", "ghc-", "nodejs-", "ocaml-", "php-", "R-", "texlive-",
        "mingw", "terra-", "kernel-", "kmod-", "akmod-",
    ];
    SUFFIXES.iter().any(|s| name.ends_with(s)) || PREFIXES.iter().any(|p| name.starts_with(p))
}
