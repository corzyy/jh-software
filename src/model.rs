//! Core data model shared by the catalog, the backends and the UI.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Desktop,
    Console,
    Addon,
    Font,
    Codec,
    Runtime,
    /// A plain package without AppStream metadata (e.g. most of Terra).
    Package,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Icon {
    /// Absolute path to a cached icon file.
    File(PathBuf),
    /// Name of a themed icon.
    Stock(String),
    /// Remote URL.
    Remote(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DescBlock {
    Para(String),
    List { ordered: bool, items: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screenshot {
    pub thumbnail: Option<String>,
    pub full: String,
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub timestamp: i64,
    pub description: Vec<DescBlock>,
}

/// Where an app can be installed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum SourceKind {
    /// RPM from the Terra repository.
    Terra,
    /// RPM from any other repository (Fedora, RPM Fusion, ...).
    Rpm,
    Flatpak,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Terra => "Terra",
            SourceKind::Rpm => "RPM",
            SourceKind::Flatpak => "Flatpak",
        }
    }

    pub fn is_rpm(self) -> bool {
        matches!(self, SourceKind::Terra | SourceKind::Rpm)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    /// RPM package name, or the full flatpak ref (`app/org.foo.Bar/x86_64/stable`).
    pub id: String,
    /// Repository id / flatpak remote name.
    pub origin: String,
    /// For flatpak: true when it belongs to the per-user installation.
    pub user: bool,
}

impl Source {
    pub fn display_origin(&self) -> String {
        match self.kind {
            SourceKind::Flatpak => {
                let scope = if self.user { "user" } else { "system" };
                format!("{} ({scope})", pretty_origin(&self.origin))
            }
            _ => pretty_origin(&self.origin),
        }
    }

    /// Release channel of an RPM build, e.g. "Nightly" for `prismlauncher-nightly`.
    pub fn channel(&self) -> Option<&'static str> {
        if !self.kind.is_rpm() {
            return None;
        }
        split_channel(&self.id).map(|(_, c)| c)
    }

    pub fn flatpak_name(&self) -> Option<&str> {
        if self.kind != SourceKind::Flatpak {
            return None;
        }
        self.id.split('/').nth(1)
    }
}

const CHANNELS: &[(&str, &str)] = &[
    ("-nightly", "Nightly"),
    ("-git", "Git"),
    ("-canary", "Canary"),
    ("-beta", "Beta"),
    ("-preview", "Preview"),
    ("-ptb", "PTB"),
    ("-bin", "Binary"),
    ("-stable", "Stable"),
];

/// Split a package name like `prismlauncher-nightly` into (`prismlauncher`, "Nightly").
pub fn split_channel(pkg: &str) -> Option<(&str, &'static str)> {
    CHANNELS
        .iter()
        .find_map(|(suffix, label)| pkg.strip_suffix(suffix).filter(|b| !b.is_empty()).map(|b| (b, *label)))
}

pub fn pretty_origin(origin: &str) -> String {
    match origin {
        "fedora" => "Fedora".into(),
        "updates" => "Fedora Updates".into(),
        "updates-testing" => "Fedora Updates Testing".into(),
        "terra" => "Terra".into(),
        "flathub" => "Flathub".into(),
        "flathub-beta" => "Flathub Beta".into(),
        o if o.starts_with("rpmfusion-free") => "RPM Fusion Free".into(),
        o if o.starts_with("rpmfusion-nonfree") => "RPM Fusion Nonfree".into(),
        o if o.starts_with("rpmfusion") => "RPM Fusion".into(),
        o => o.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct App {
    /// Normalized AppStream id (no `.desktop` suffix), or `pkg:<name>` for bare packages.
    pub id: String,
    pub kind: AppKind,
    pub name: String,
    pub summary: String,
    pub description: Vec<DescBlock>,
    pub developer: Option<String>,
    pub icon: Option<Icon>,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    pub license: Option<String>,
    pub urls: Vec<(String, String)>,
    pub screenshots: Vec<Screenshot>,
    pub releases: Vec<Release>,
    pub content_rating: Vec<(String, String)>,
    pub launchable: Option<String>,
    pub verified: bool,
    pub featured: bool,
    pub popular: bool,
    /// Ids this component extends (for add-ons).
    pub extends: Vec<String>,
    pub sources: Vec<Source>,
}

impl App {
    pub fn url(&self, kind: &str) -> Option<&str> {
        self.urls.iter().find(|(k, _)| k == kind).map(|(_, v)| v.as_str())
    }

    pub fn latest_release(&self) -> Option<&Release> {
        self.releases.iter().max_by_key(|r| r.timestamp)
    }

    pub fn is_free(&self) -> bool {
        match &self.license {
            None => true,
            Some(l) => !(l.contains("LicenseRef-proprietary") || l.eq_ignore_ascii_case("proprietary")),
        }
    }

    /// Plain-text rendering of the description, used for search.
    pub fn description_text(&self) -> String {
        let mut s = String::new();
        for b in &self.description {
            match b {
                DescBlock::Para(p) => {
                    s.push_str(p);
                    s.push('\n');
                }
                DescBlock::List { items, .. } => {
                    for i in items {
                        s.push_str(i);
                        s.push('\n');
                    }
                }
            }
        }
        s
    }
}

/// Human readable byte size.
pub fn format_size(bytes: u64) -> String {
    glib_format_size(bytes)
}

fn glib_format_size(bytes: u64) -> String {
    gtk::glib::format_size(bytes).to_string()
}
