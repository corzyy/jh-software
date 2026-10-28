//! Merged software catalog built from all AppStream sources plus Terra.
//!
//! Building parses ~60 MB of XML, so the result is cached with bincode and
//! only rebuilt when one of the input files changes.

use crate::appstream::{self, Component, IconBase, Locales};
use crate::backend::{flatpak, terra};
use crate::model::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

const CACHE_VERSION: u32 = 5;

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    fingerprint: Vec<(PathBuf, u64, u64)>,
    locale: String,
    apps: Vec<App>,
}

/// One input file for the catalog.
#[derive(Debug, Clone)]
struct Input {
    path: PathBuf,
    icons: Option<IconBase>,
    flatpak: Option<(String, bool)>, // (remote, user)
}

pub struct Catalog {
    pub apps: Vec<Arc<App>>,
    by_id: HashMap<String, usize>,
    by_pkg: HashMap<String, usize>,
    by_flatpak: HashMap<String, usize>,
    addons: HashMap<String, Vec<usize>>,
    search: Vec<SearchEntry>,
}

struct SearchEntry {
    name: String,
    id: String,
    summary: String,
    keywords: String,
    pkgnames: String,
    description: String,
}

pub fn normalize_id(id: &str) -> String {
    id.strip_suffix(".desktop").unwrap_or(id).to_string()
}

fn mtime_size(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some((t, m.len()))
}

fn xml_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.to_string_lossy();
                    n.ends_with(".xml") || n.ends_with(".xml.gz")
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn discover_inputs() -> Vec<Input> {
    let mut inputs = Vec::new();
    for (xml, icons) in [
        ("/usr/share/swcatalog/xml", "/usr/share/swcatalog/icons"),
        ("/var/cache/swcatalog/xml", "/var/cache/swcatalog/icons"),
        ("/usr/share/app-info/xmls", "/usr/share/app-info/icons"),
    ] {
        for p in xml_files_in(Path::new(xml)) {
            // Prefer an uncompressed sibling if both exist.
            inputs.push(Input { path: p, icons: Some(IconBase::PerOrigin(PathBuf::from(icons))), flatpak: None });
        }
    }
    for r in flatpak::list_remotes() {
        if r.disabled {
            continue;
        }
        let Some(dir) = r.appstream_dir else { continue };
        let plain = dir.join("appstream.xml");
        let gz = dir.join("appstream.xml.gz");
        let path = if plain.exists() { plain } else if gz.exists() { gz } else { continue };
        inputs.push(Input { path, icons: Some(IconBase::Flat(dir.join("icons"))), flatpak: Some((r.name, r.user)) });
    }
    inputs
}

fn fingerprint(inputs: &[Input], terra_files: &[PathBuf]) -> Vec<(PathBuf, u64, u64)> {
    inputs
        .iter()
        .map(|i| &i.path)
        .chain(terra_files.iter())
        .filter_map(|p| mtime_size(p).map(|(t, s)| (p.clone(), t, s)))
        .collect()
}

fn cache_path() -> PathBuf {
    crate::config::cache_dir().join("catalog.bin")
}

/// Load the cached catalog if it is still valid.
pub fn load_cached() -> Option<Catalog> {
    let data = std::fs::read(cache_path()).ok()?;
    let cache: CacheFile = bincode::deserialize(&data).ok()?;
    if cache.version != CACHE_VERSION {
        return None;
    }
    Some(Catalog::from_apps(cache.apps))
}

/// True when the cache no longer matches the input files.
pub fn is_stale() -> bool {
    let inputs = discover_inputs();
    let fp = fingerprint(&inputs, &terra::metadata_files());
    let locale = Locales::from_env().key();
    let Ok(data) = std::fs::read(cache_path()) else { return true };
    // Only decode the header fields.
    #[derive(Deserialize)]
    struct Header {
        version: u32,
        fingerprint: Vec<(PathBuf, u64, u64)>,
        locale: String,
    }
    match bincode::deserialize::<Header>(&data) {
        Ok(h) => h.version != CACHE_VERSION || h.fingerprint != fp || h.locale != locale,
        Err(_) => true,
    }
}

/// Parse every source and build a fresh catalog (slow; run in a thread).
pub fn build() -> Catalog {
    let locales = Locales::from_env();
    let inputs = discover_inputs();
    let terra_files = terra::metadata_files();
    let fp = fingerprint(&inputs, &terra_files);

    // Parse all files in parallel.
    let parsed: Vec<(Input, Option<String>, Vec<Component>)> = std::thread::scope(|s| {
        let handles: Vec<_> = inputs
            .iter()
            .cloned()
            .map(|input| {
                let locales = &locales;
                s.spawn(move || {
                    match appstream::parse_file(&input.path, input.icons.as_ref(), locales) {
                        Ok((origin, comps)) => (input, origin, comps),
                        Err(e) => {
                            eprintln!("catalog: failed to parse {}: {e}", input.path.display());
                            (input, None, Vec::new())
                        }
                    }
                })
            })
            .collect();
        let terra_handle = s.spawn(terra::list);
        let mut v: Vec<_> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
        let terra_pkgs = terra_handle.join().unwrap_or_default();
        v.push((Input { path: PathBuf::new(), icons: None, flatpak: None }, Some("__terra__".into()), terra_to_components(terra_pkgs)));
        v
    });

    let apps = merge(parsed);
    let cat = Catalog::from_apps(apps.clone());

    let cache = CacheFile { version: CACHE_VERSION, fingerprint: fp, locale: locales.key(), apps };
    if let Ok(data) = bincode::serialize(&cache) {
        let dir = crate::config::cache_dir();
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join("catalog.bin.tmp");
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(tmp, cache_path());
        }
    }
    cat
}

fn terra_to_components(pkgs: Vec<terra::TerraPackage>) -> Vec<Component> {
    pkgs.into_iter()
        .filter(|p| !terra::is_noise(&p.name))
        .map(|p| Component {
            id: p.name.clone(),
            kind: "__terra__".into(),
            pkgname: Some(p.name.clone()),
            name: p.name,
            summary: p.summary,
            license: Some(p.license).filter(|l| !l.is_empty()),
            urls: if p.url.is_empty() { vec![] } else { vec![("homepage".into(), p.url)] },
            ..Default::default()
        })
        .collect()
}

fn kind_of(t: &str) -> AppKind {
    match t {
        "desktop" | "desktop-application" => AppKind::Desktop,
        "console-application" => AppKind::Console,
        "addon" => AppKind::Addon,
        "font" => AppKind::Font,
        "codec" => AppKind::Codec,
        "runtime" => AppKind::Runtime,
        _ => AppKind::Package,
    }
}

fn component_to_app(c: Component, sources: Vec<Source>) -> App {
    let verified = c.custom.iter().any(|(k, v)| k == "flathub::verification::verified" && v == "true");
    App {
        id: normalize_id(&c.id),
        kind: kind_of(&c.kind),
        name: if c.name.is_empty() { c.id.clone() } else { c.name },
        summary: c.summary,
        description: c.description,
        developer: c.developer,
        icon: c.icon,
        categories: c.categories,
        keywords: c.keywords,
        license: c.license,
        urls: c.urls,
        screenshots: c.screenshots,
        releases: c.releases,
        content_rating: c.content_rating,
        launchable: c.launchable,
        verified,
        featured: false,
        popular: false,
        extends: c.extends.iter().map(|e| normalize_id(e)).collect(),
        sources,
    }
}

/// Fill in missing fields of `dst` from `src`.
fn merge_meta(dst: &mut App, src: &App) {
    if dst.summary.is_empty() {
        dst.summary = src.summary.clone();
    }
    if dst.description.is_empty() {
        dst.description = src.description.clone();
    }
    if dst.developer.is_none() {
        dst.developer = src.developer.clone();
    }
    // Prefer local icon files over stock/remote.
    match (&dst.icon, &src.icon) {
        (None, Some(_)) | (Some(Icon::Stock(_)), Some(Icon::File(_))) | (Some(Icon::Remote(_)), Some(Icon::File(_))) => {
            dst.icon = src.icon.clone()
        }
        _ => {}
    }
    if dst.screenshots.is_empty() {
        dst.screenshots = src.screenshots.clone();
    }
    if dst.releases.is_empty() {
        dst.releases = src.releases.clone();
    }
    for c in &src.categories {
        if !dst.categories.contains(c) {
            dst.categories.push(c.clone());
        }
    }
    for k in &src.keywords {
        if !dst.keywords.contains(k) {
            dst.keywords.push(k.clone());
        }
    }
    for u in &src.urls {
        if !dst.urls.iter().any(|(k, _)| *k == u.0) {
            dst.urls.push(u.clone());
        }
    }
    if dst.license.is_none() {
        dst.license = src.license.clone();
    }
    if dst.content_rating.is_empty() {
        dst.content_rating = src.content_rating.clone();
    }
    if dst.launchable.is_none() {
        dst.launchable = src.launchable.clone();
    }
    dst.verified |= src.verified;
    for s in &src.sources {
        let dup = dst.sources.iter().any(|d| d == s || (d.kind.is_rpm() && d.kind == s.kind && d.id == s.id));
        if !dup {
            dst.sources.push(s.clone());
        }
    }
}

fn merge(parsed: Vec<(Input, Option<String>, Vec<Component>)>) -> Vec<App> {
    let mut apps: Vec<App> = Vec::with_capacity(16_000);
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut appends: Vec<Component> = Vec::new();
    let mut terra: Vec<Component> = Vec::new();

    // Flatpak metadata is usually the richest and most current, so it goes first
    // and becomes the primary metadata. RPM catalogs then add their sources.
    let mut ordered = parsed;
    ordered.sort_by_key(|(i, o, _)| match (&i.flatpak, o.as_deref()) {
        (Some(_), _) => 0,
        (_, Some("__terra__")) => 3,
        _ => 1,
    });

    for (input, origin, comps) in ordered {
        let origin = origin.unwrap_or_default();
        if origin == "__terra__" {
            terra = comps;
            continue;
        }
        for c in comps {
            if c.merge.is_some() {
                appends.push(c);
                continue;
            }
            let source = if let Some((remote, user)) = &input.flatpak {
                let Some(bundle) = c.bundle.clone() else { continue };
                Source { kind: SourceKind::Flatpak, id: bundle, origin: remote.clone(), user: *user }
            } else if let Some(pkg) = c.pkgname.clone() {
                let kind = if terra::is_terra_repo(&origin) { SourceKind::Terra } else { SourceKind::Rpm };
                Source { kind, id: pkg, origin: origin.clone(), user: false }
            } else {
                // No way to install it (repository descriptions, web apps…).
                continue;
            };
            // Skip flatpak runtimes / extensions that are not user-facing apps.
            if source.kind == SourceKind::Flatpak && kind_of(&c.kind) == AppKind::Runtime {
                continue;
            }

            let id = normalize_id(&c.id);
            let provides: Vec<String> = c.provides.iter().map(|p| normalize_id(p)).collect();
            let existing = index
                .get(&id)
                .copied()
                .or_else(|| provides.iter().find_map(|p| index.get(p).copied()));
            let app = component_to_app(c, vec![source]);
            match existing {
                Some(i) => merge_meta(&mut apps[i], &app),
                None => {
                    let i = apps.len();
                    index.insert(id, i);
                    for p in provides {
                        index.entry(p).or_insert(i);
                    }
                    if let Some(l) = &app.launchable {
                        index.entry(normalize_id(l)).or_insert(i);
                    }
                    apps.push(app);
                }
            }
        }
    }

    // GNOME's curated list: featured tiles and popular kudos.
    for c in appends {
        let Some(&i) = index.get(&normalize_id(&c.id)) else { continue };
        let a = &mut apps[i];
        if c.custom.iter().any(|(k, v)| k == "GnomeSoftware::FeatureTile" && v.eq_ignore_ascii_case("true")) {
            a.featured = true;
        }
        if c.kudos.iter().any(|k| k == "GnomeSoftware::popular") {
            a.popular = true;
        }
        for cat in c.categories {
            if cat != "Featured" && !a.categories.contains(&cat) {
                a.categories.push(cat);
            }
        }
    }

    merge_by_name(&mut apps);
    merge_terra(&mut apps, terra);
    apps
}

/// Merge RPM-only apps into flatpak-only apps when they are clearly the same
/// software but use different AppStream ids (e.g. `discord` vs `com.discordapp.Discord`).
fn merge_by_name(apps: &mut Vec<App>) {
    let has = |a: &App, rpm: bool| a.sources.iter().any(|s| s.kind.is_rpm() == rpm);
    let mut flatpak_only: HashMap<(String, String), usize> = HashMap::new();
    for (i, a) in apps.iter().enumerate() {
        if a.kind == AppKind::Desktop && has(a, false) && !has(a, true) {
            let last = a.id.rsplit('.').next().unwrap_or(&a.id).to_lowercase();
            flatpak_only.insert((last, a.name.to_lowercase()), i);
        }
    }
    let mut remove = Vec::new();
    for i in 0..apps.len() {
        let a = &apps[i];
        if a.kind != AppKind::Desktop || has(a, false) || !has(a, true) {
            continue;
        }
        let name = a.name.to_lowercase();
        let mut keys = vec![a.id.rsplit('.').next().unwrap_or(&a.id).to_lowercase()];
        keys.extend(a.sources.iter().map(|s| s.id.to_lowercase()));
        let target = keys.into_iter().find_map(|k| flatpak_only.get(&(k, name.clone())).copied());
        if let Some(t) = target {
            let src = apps[i].clone();
            merge_meta(&mut apps[t], &src);
            remove.push(i);
        }
    }
    for i in remove.into_iter().rev() {
        apps.remove(i);
    }
}

/// Attach Terra packages to existing AppStream apps where they clearly match,
/// otherwise add them as plain packages.
fn merge_terra(apps: &mut Vec<App>, terra: Vec<Component>) {
    let mut rpm_names: HashMap<String, ()> = HashMap::new();
    let mut by_slug: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, a) in apps.iter().enumerate() {
        for s in &a.sources {
            if s.kind.is_rpm() {
                rpm_names.insert(s.id.clone(), ());
            }
        }
        if a.kind != AppKind::Desktop {
            continue;
        }
        let last = a.id.rsplit('.').next().unwrap_or(&a.id).to_lowercase();
        by_slug.entry(last).or_default().push(i);
        let name_slug = a.name.to_lowercase().replace(' ', "-");
        by_slug.entry(name_slug.clone()).or_default().push(i);
        by_slug.entry(name_slug.replace('-', "")).or_default().push(i);
    }

    for c in terra {
        let pkg = c.pkgname.clone().unwrap_or_default();
        let source = Source { kind: SourceKind::Terra, id: pkg.clone(), origin: terra::REPO_ID.into(), user: false };
        if rpm_names.contains_key(&pkg) {
            // Same package name already offered via another RPM repo; dnf picks the version.
            for a in apps.iter_mut() {
                if a.sources.iter().any(|s| s.kind.is_rpm() && s.id == pkg) && !a.sources.contains(&source) {
                    a.sources.push(source.clone());
                }
            }
            continue;
        }
        let key = pkg.to_lowercase();
        let mut uniq: Vec<usize> = by_slug.get(&key).cloned().unwrap_or_default();
        uniq.sort();
        uniq.dedup();
        // Exact name: only attach to apps that have no RPM yet.
        let mut target = uniq.into_iter().find(|&i| !apps[i].sources.iter().any(|s| s.kind.is_rpm()));
        // Channel builds (`prismlauncher-nightly`, `discord-canary`, `localsend-bin`)
        // belong to the app of the base name, next to any existing sources.
        if target.is_none() {
            if let Some((base, _)) = split_channel(&key) {
                let mut cands: Vec<usize> = by_slug.get(base).cloned().unwrap_or_default();
                cands.extend(
                    apps.iter()
                        .enumerate()
                        .filter(|(_, a)| a.kind == AppKind::Desktop && a.sources.iter().any(|s| s.kind.is_rpm() && s.id == base))
                        .map(|(i, _)| i),
                );
                cands.sort();
                cands.dedup();
                target = cands.into_iter().find(|&i| !apps[i].sources.iter().any(|s| s.kind.is_rpm() && s.id == pkg));
            }
        }
        match target {
            Some(i) => apps[i].sources.push(source),
            None => {
                let mut app = component_to_app(c, vec![source]);
                app.id = format!("pkg:{pkg}");
                app.kind = AppKind::Package;
                apps.push(app);
            }
        }
    }
}

impl Catalog {
    pub fn empty() -> Self {
        Self::from_apps(Vec::new())
    }

    fn from_apps(apps: Vec<App>) -> Self {
        let apps: Vec<Arc<App>> = apps.into_iter().map(Arc::new).collect();
        let mut by_id = HashMap::with_capacity(apps.len());
        let mut by_pkg = HashMap::new();
        let mut by_flatpak = HashMap::new();
        let mut addons: HashMap<String, Vec<usize>> = HashMap::new();
        let mut search = Vec::with_capacity(apps.len());
        for (i, a) in apps.iter().enumerate() {
            by_id.insert(a.id.clone(), i);
            if let Some(l) = &a.launchable {
                by_id.entry(normalize_id(l)).or_insert(i);
            }
            for s in &a.sources {
                match s.kind {
                    SourceKind::Flatpak => {
                        if let Some(n) = s.flatpak_name() {
                            by_flatpak.entry(n.to_string()).or_insert(i);
                        }
                    }
                    _ => {
                        // Desktop apps win over add-ons/fonts sharing a package.
                        let replace = match by_pkg.get(&s.id) {
                            None => true,
                            Some(&j) => {
                                let other: &Arc<App> = &apps[j];
                                other.kind != AppKind::Desktop && a.kind == AppKind::Desktop
                            }
                        };
                        if replace {
                            by_pkg.insert(s.id.clone(), i);
                        }
                    }
                }
            }
            for e in &a.extends {
                addons.entry(e.clone()).or_default().push(i);
            }
            search.push(SearchEntry {
                name: a.name.to_lowercase(),
                id: a.id.to_lowercase(),
                summary: a.summary.to_lowercase(),
                keywords: a.keywords.join(" "),
                pkgnames: a.sources.iter().filter(|s| s.kind.is_rpm()).map(|s| s.id.to_lowercase()).collect::<Vec<_>>().join(" "),
                description: a.description_text().to_lowercase(),
            });
        }
        Catalog { apps, by_id, by_pkg, by_flatpak, addons, search }
    }

    pub fn len(&self) -> usize {
        self.apps.len()
    }

    pub fn get(&self, id: &str) -> Option<&Arc<App>> {
        self.by_id.get(id).or_else(|| self.by_id.get(&normalize_id(id))).map(|&i| &self.apps[i])
    }

    pub fn by_package(&self, name: &str) -> Option<&Arc<App>> {
        self.by_pkg.get(name).map(|&i| &self.apps[i])
    }

    pub fn by_flatpak_name(&self, name: &str) -> Option<&Arc<App>> {
        self.by_flatpak.get(name).map(|&i| &self.apps[i]).or_else(|| self.get(name))
    }

    pub fn addons_for(&self, id: &str) -> Vec<Arc<App>> {
        self.addons.get(id).map(|v| v.iter().map(|&i| self.apps[i].clone()).collect()).unwrap_or_default()
    }

    /// Apps shown in browse views (have an icon, are desktop apps).
    pub fn is_browsable(app: &App) -> bool {
        app.kind == AppKind::Desktop && app.icon.is_some() && !app.sources.is_empty()
    }

    pub fn search(&self, query: &str, include_packages: bool, limit: usize) -> Vec<Arc<App>> {
        let tokens: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        if tokens.is_empty() {
            return Vec::new();
        }
        let full = tokens.join(" ");
        let mut scored: Vec<(i32, usize)> = Vec::new();
        'apps: for (i, e) in self.search.iter().enumerate() {
            let app = &self.apps[i];
            if !include_packages && app.kind == AppKind::Package {
                continue;
            }
            let mut score = 0i32;
            for t in &tokens {
                let s = if e.name == *t {
                    100
                } else if e.name.starts_with(t.as_str()) {
                    70
                } else if e.name.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(t.as_str())) {
                    55
                } else if e.name.contains(t.as_str()) {
                    40
                } else if e.pkgnames.split(' ').any(|p| p == t) {
                    45
                } else if e.keywords.contains(t.as_str()) {
                    30
                } else if e.id.contains(t.as_str()) || e.pkgnames.contains(t.as_str()) {
                    25
                } else if e.summary.contains(t.as_str()) {
                    15
                } else if e.description.contains(t.as_str()) {
                    5
                } else {
                    continue 'apps;
                };
                score += s;
            }
            if e.name == full {
                score += 100;
            }
            score += match app.kind {
                AppKind::Desktop => 20,
                AppKind::Console => 5,
                AppKind::Package => -25,
                _ => 0,
            };
            if app.icon.is_some() {
                score += 5;
            }
            if app.featured || app.popular {
                score += 8;
            }
            if app.verified {
                score += 3;
            }
            scored.push((score, i));
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| self.apps[a.1].name.cmp(&self.apps[b.1].name)));
        scored.into_iter().take(limit).map(|(_, i)| self.apps[i].clone()).collect()
    }
}

/// Top-level categories, mirroring GNOME Software.
pub struct CategoryDef {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub matches: &'static [&'static str],
}

pub const CATEGORIES: &[CategoryDef] = &[
    CategoryDef {
        id: "create",
        name: "Create",
        icon: "applications-graphics-symbolic",
        matches: &["Graphics", "AudioVideoEditing", "Photography", "3DGraphics", "VectorGraphics", "RasterGraphics", "Publishing", "Music", "Recorder"],
    },
    CategoryDef {
        id: "work",
        name: "Work",
        icon: "x-office-document-symbolic",
        matches: &["Office", "Finance", "ProjectManagement", "WordProcessor", "Spreadsheet", "Presentation", "Calendar", "ContactManagement", "Database", "Viewer"],
    },
    CategoryDef { id: "play", name: "Play", icon: "input-gaming-symbolic", matches: &["Game"] },
    CategoryDef {
        id: "socialize",
        name: "Socialize",
        icon: "user-available-symbolic",
        matches: &["Chat", "InstantMessaging", "Email", "Feed", "IRCClient", "VideoConference", "Telephony", "News", "WebBrowser", "P2P"],
    },
    CategoryDef {
        id: "learn",
        name: "Learn",
        icon: "accessories-dictionary-symbolic",
        matches: &["Education", "Science", "Math", "Astronomy", "Languages", "Dictionary", "Geography", "Geoscience", "History", "Literature", "Chemistry", "Biology", "Physics"],
    },
    CategoryDef {
        id: "develop",
        name: "Develop",
        icon: "applications-engineering-symbolic",
        matches: &["Development", "IDE", "Debugger", "RevisionControl", "WebDevelopment", "GUIDesigner", "Building", "Profiling", "Translation"],
    },
    CategoryDef {
        id: "media",
        name: "Audio & Video",
        icon: "applications-multimedia-symbolic",
        matches: &["AudioVideo", "Audio", "Video", "Player", "TV", "Midi", "Mixer", "Tuner"],
    },
    CategoryDef {
        id: "utilities",
        name: "Utilities",
        icon: "applications-utilities-symbolic",
        matches: &["Utility", "System", "Settings", "Accessibility", "FileTools", "FileManager", "Monitor", "Archiving", "Security", "TerminalEmulator", "TextEditor", "Network"],
    },
];

pub fn category(id: &str) -> Option<&'static CategoryDef> {
    CATEGORIES.iter().find(|c| c.id == id)
}

impl Catalog {
    pub fn in_category(&self, def: &CategoryDef, show_nonfree: bool) -> Vec<Arc<App>> {
        let mut v: Vec<Arc<App>> = self
            .apps
            .iter()
            .filter(|a| Self::is_browsable(a) && (show_nonfree || a.is_free()))
            .filter(|a| a.categories.iter().any(|c| def.matches.contains(&c.as_str())))
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            let rank = |x: &App| -> i32 {
                (x.featured as i32) * 4 + (x.popular as i32) * 4 + (x.verified as i32) * 2 + (!x.screenshots.is_empty()) as i32
            };
            rank(b).cmp(&rank(a)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        v
    }

    pub fn featured(&self, show_nonfree: bool) -> Vec<Arc<App>> {
        self.apps
            .iter()
            .filter(|a| a.featured && Self::is_browsable(a) && (show_nonfree || a.is_free()))
            .cloned()
            .collect()
    }

    pub fn popular(&self, show_nonfree: bool) -> Vec<Arc<App>> {
        self.apps
            .iter()
            .filter(|a| a.popular && Self::is_browsable(a) && (show_nonfree || a.is_free()))
            .cloned()
            .collect()
    }

    /// Recently released apps with good metadata.
    pub fn new_and_updated(&self, show_nonfree: bool, limit: usize) -> Vec<Arc<App>> {
        let mut v: Vec<&Arc<App>> = self
            .apps
            .iter()
            .filter(|a| Self::is_browsable(a) && (show_nonfree || a.is_free()) && !a.screenshots.is_empty() && a.description.len() > 1)
            .filter(|a| a.latest_release().is_some())
            .collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.latest_release().map(|r| r.timestamp).unwrap_or(0)));
        v.into_iter().take(limit).cloned().collect()
    }

    pub fn by_developer(&self, developer: &str, exclude: &str, limit: usize) -> Vec<Arc<App>> {
        self.apps
            .iter()
            .filter(|a| a.id != exclude && a.developer.as_deref() == Some(developer) && Self::is_browsable(a))
            .take(limit)
            .cloned()
            .collect()
    }
}
