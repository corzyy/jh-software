//! Fast streaming parser for AppStream catalog XML (swcatalog / flatpak appstream).
//!
//! Only the fields the store needs are extracted. Translations are resolved
//! while parsing against the user's locale so the cache stays small.

use crate::model::{DescBlock, Icon, Release, Screenshot};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

/// A raw component as found in one catalog file.
#[derive(Debug, Default, Clone)]
pub struct Component {
    pub id: String,
    pub kind: String,
    pub merge: Option<String>,
    pub pkgname: Option<String>,
    pub bundle: Option<String>,
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
    pub provides: Vec<String>,
    pub extends: Vec<String>,
    pub custom: Vec<(String, String)>,
    pub kudos: Vec<String>,
}

/// How to resolve `<icon type="cached">` entries.
#[derive(Debug, Clone)]
pub enum IconBase {
    /// `<base>/<origin>/<W>x<H>/<name>` (swcatalog layout).
    PerOrigin(PathBuf),
    /// `<base>/<W>x<H>/<name>` (flatpak appstream layout).
    Flat(PathBuf),
}

/// Locale preference, e.g. `["de_DE", "de"]`.
#[derive(Debug, Clone)]
pub struct Locales(pub Vec<String>);

impl Locales {
    pub fn from_env() -> Self {
        let raw = std::env::var("LC_ALL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("LANG").ok())
            .unwrap_or_default();
        let base = raw.split(['.', '@']).next().unwrap_or("").to_string();
        let mut v = Vec::new();
        if !base.is_empty() && base != "C" && base != "POSIX" {
            v.push(base.clone());
            if let Some((lang, _)) = base.split_once('_') {
                v.push(lang.to_string());
            }
        }
        Locales(v)
    }

    pub fn key(&self) -> String {
        self.0.join(",")
    }

    /// Higher is better. 0 means "not usable".
    fn score(&self, lang: Option<&str>) -> u8 {
        match lang {
            None | Some("") | Some("C") | Some("en") | Some("en_US") => 1,
            Some(l) => {
                for (i, want) in self.0.iter().enumerate() {
                    if want == l {
                        return 10 - i as u8;
                    }
                }
                0
            }
        }
    }
}

struct Translatable {
    value: String,
    score: u8,
}

impl Translatable {
    fn new() -> Self {
        Translatable { value: String::new(), score: 0 }
    }
    fn offer(&mut self, score: u8, v: String) {
        if (score > self.score || (score == self.score && self.value.is_empty()))
            && score > 0 {
                self.value = v;
                self.score = score;
            }
    }
}

/// Per-language description accumulator.
#[derive(Default)]
struct DescAcc {
    blocks: HashMap<u8, Vec<DescBlock>>,
    list: Option<(bool, HashMap<u8, Vec<String>>)>,
}

impl DescAcc {
    fn para(&mut self, score: u8, text: String) {
        if score == 0 || text.is_empty() {
            return;
        }
        self.blocks.entry(score).or_default().push(DescBlock::Para(text));
    }
    fn start_list(&mut self, ordered: bool) {
        self.list = Some((ordered, HashMap::new()));
    }
    fn item(&mut self, score: u8, text: String) {
        if score == 0 || text.is_empty() {
            return;
        }
        if let Some((_, items)) = &mut self.list {
            items.entry(score).or_default().push(text);
        } else {
            self.para(score, text);
        }
    }
    fn end_list(&mut self) {
        if let Some((ordered, items)) = self.list.take() {
            for (score, items) in items {
                self.blocks.entry(score).or_default().push(DescBlock::List { ordered, items });
            }
        }
    }
    fn finish(mut self) -> Vec<DescBlock> {
        self.end_list();
        let best = self.blocks.keys().copied().max();
        best.and_then(|b| self.blocks.remove(&b)).unwrap_or_default()
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        if a.key.local_name().as_ref() == name {
            return Some(
                a.unescape_value()
                    .map(|v| v.into_owned())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned()),
            );
        }
    }
    None
}

/// Convert `YYYY-MM-DD` (optionally with time) to a unix timestamp.
fn parse_date(s: &str) -> Option<i64> {
    let d = s.get(0..10)?;
    let mut it = d.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let day: i64 = it.next()?.parse().ok()?;
    // days_from_civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400)
}

#[derive(Default)]
struct IconCandidate {
    kind: String,
    width: u32,
    scale: u32,
    value: String,
}

const MAX_RELEASES: usize = 8;

/// Parse a catalog file (plain or gzip compressed).
pub fn parse_file(path: &Path, icon_base: Option<&IconBase>, locales: &Locales) -> anyhow::Result<(Option<String>, Vec<Component>)> {
    let file = std::fs::File::open(path)?;
    let is_gz = path.extension().is_some_and(|e| e == "gz");
    if is_gz {
        let dec = flate2::read::GzDecoder::new(file);
        parse_reader(BufReader::with_capacity(256 * 1024, dec), icon_base, locales)
    } else {
        parse_reader(BufReader::with_capacity(256 * 1024, file), icon_base, locales)
    }
}

pub fn parse_reader<R: BufRead>(input: R, icon_base: Option<&IconBase>, locales: &Locales) -> anyhow::Result<(Option<String>, Vec<Component>)> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = false;

    let mut buf = Vec::with_capacity(4096);
    let mut out = Vec::new();
    let mut origin: Option<String> = None;

    // Element stack (local names) for context.
    let mut stack: Vec<Vec<u8>> = Vec::with_capacity(16);
    let mut text = String::new();

    let mut comp: Option<Component> = None;
    let mut name = Translatable::new();
    let mut summary = Translatable::new();
    let mut developer = Translatable::new();
    let mut desc = DescAcc::default();
    let mut desc_lang: Option<String> = None;
    let mut elem_lang: Option<String> = None;
    let mut icons: Vec<IconCandidate> = Vec::new();
    let mut cur_icon = IconCandidate::default();
    let mut url_kind = String::new();
    let mut launch_kind = String::new();
    let mut custom_key = String::new();
    let mut rating_id = String::new();
    let mut bundle_kind = String::new();
    let mut shot: Option<(Option<String>, Option<(u32, String)>, Option<String>, Translatable)> = None;
    let mut img_kind = String::new();
    let mut img_width = 0u32;
    let mut release: Option<(Release, DescAcc)> = None;
    let mut keyword_ok = true;

    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => {
                // Be tolerant: return what we have so far.
                eprintln!("appstream: parse error at {}: {e}", reader.buffer_position());
                break;
            }
        };
        match ev {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(ev, Event::Empty(_));
                let local = e.local_name().as_ref().to_vec();
                let parent = stack.last().map(|v| v.as_slice());
                let in_release = release.is_some();
                match local.as_slice() {
                    b"components" => origin = attr(e, b"origin"),
                    b"component" if comp.is_none() => {
                        comp = Some(Component {
                            kind: attr(e, b"type").unwrap_or_else(|| "generic".into()),
                            merge: attr(e, b"merge"),
                            ..Default::default()
                        });
                        name = Translatable::new();
                        summary = Translatable::new();
                        developer = Translatable::new();
                        desc = DescAcc::default();
                        icons.clear();
                    }
                    b"description" => {
                        desc_lang = attr(e, b"lang");
                        if in_release
                            && let Some((_, d)) = &mut release {
                                *d = DescAcc::default();
                            }
                    }
                    b"p" | b"li" => {
                        elem_lang = attr(e, b"lang").or_else(|| desc_lang.clone());
                        text.clear();
                    }
                    b"ul" | b"ol" => {
                        let ordered = local.as_slice() == b"ol";
                        if let Some((_, d)) = &mut release {
                            d.start_list(ordered);
                        } else {
                            desc.start_list(ordered);
                        }
                    }
                    b"icon" if parent == Some(b"component") => {
                        cur_icon = IconCandidate {
                            kind: attr(e, b"type").unwrap_or_default(),
                            width: attr(e, b"width").and_then(|w| w.parse().ok()).unwrap_or(0),
                            scale: attr(e, b"scale").and_then(|w| w.parse().ok()).unwrap_or(1),
                            value: String::new(),
                        };
                        text.clear();
                    }
                    b"url" => {
                        url_kind = attr(e, b"type").unwrap_or_else(|| "homepage".into());
                        text.clear();
                    }
                    b"launchable" => {
                        launch_kind = attr(e, b"type").unwrap_or_default();
                        text.clear();
                    }
                    b"value" => {
                        custom_key = attr(e, b"key").unwrap_or_default();
                        text.clear();
                    }
                    b"content_attribute" => {
                        rating_id = attr(e, b"id").unwrap_or_default();
                        text.clear();
                    }
                    b"bundle" => {
                        bundle_kind = attr(e, b"type").unwrap_or_default();
                        text.clear();
                    }
                    b"keyword" => {
                        keyword_ok = locales.score(attr(e, b"lang").as_deref()) > 0;
                        text.clear();
                    }
                    b"screenshot" => {
                        shot = Some((attr(e, b"type"), None, None, Translatable::new()));
                    }
                    b"image" => {
                        img_kind = attr(e, b"type").unwrap_or_else(|| "source".into());
                        img_width = attr(e, b"width").and_then(|w| w.parse().ok()).unwrap_or(0);
                        text.clear();
                    }
                    b"caption" => {
                        elem_lang = attr(e, b"lang");
                        text.clear();
                    }
                    b"release" if parent == Some(b"releases") => {
                        let ts = attr(e, b"timestamp")
                            .and_then(|t| t.parse().ok())
                            .or_else(|| attr(e, b"date").and_then(|d| parse_date(&d)))
                            .unwrap_or(0);
                        let r = Release {
                            version: attr(e, b"version").unwrap_or_default(),
                            timestamp: ts,
                            description: Vec::new(),
                        };
                        if is_empty {
                            if let Some(c) = &mut comp {
                                c.releases.push(r);
                            }
                        } else {
                            release = Some((r, DescAcc::default()));
                        }
                    }
                    b"name" | b"summary" | b"developer_name" => {
                        elem_lang = attr(e, b"lang");
                        text.clear();
                    }
                    _ => {
                        // Inline markup (<em>, <code>) must keep accumulating text.
                        if !matches!(local.as_slice(), b"em" | b"code") {
                            text.clear();
                        }
                    }
                }
                if !is_empty {
                    stack.push(local);
                }
            }
            Event::Text(t) => {
                match t.unescape() {
                    Ok(s) => text.push_str(&s),
                    Err(_) => text.push_str(&String::from_utf8_lossy(&t)),
                }
            }
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::End(_) => {
                let Some(local) = stack.pop() else { continue };
                let parent = stack.last().map(|v| v.as_slice());
                if local.as_slice() == b"component" && parent == Some(b"components") || (local.as_slice() == b"component" && stack.is_empty()) {
                    if let Some(mut c) = comp.take() {
                        c.name = std::mem::take(&mut name.value);
                        c.summary = std::mem::take(&mut summary.value);
                        if !developer.value.is_empty() {
                            c.developer = Some(std::mem::take(&mut developer.value));
                        }
                        c.description = std::mem::take(&mut desc).finish();
                        c.icon = pick_icon(&icons, icon_base, origin.as_deref());
                        c.releases.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                        c.releases.truncate(MAX_RELEASES);
                        if !c.id.is_empty() {
                            out.push(c);
                        }
                    }
                    buf.clear();
                    continue;
                }
                let Some(c) = comp.as_mut() else {
                    buf.clear();
                    continue;
                };
                match local.as_slice() {
                    b"id" if parent == Some(b"component") => c.id = text.trim().to_string(),
                    b"id" if parent == Some(b"provides") => c.provides.push(text.trim().to_string()),
                    b"id" if parent == Some(b"extends") => c.extends.push(text.trim().to_string()),
                    b"pkgname" => c.pkgname = Some(text.trim().to_string()),
                    b"name" if parent == Some(b"component") => {
                        name.offer(locales.score(elem_lang.as_deref()), collapse_ws(&text))
                    }
                    b"name" if parent == Some(b"developer") => {
                        developer.offer(locales.score(elem_lang.as_deref()), collapse_ws(&text))
                    }
                    b"developer_name" => developer.offer(locales.score(elem_lang.as_deref()), collapse_ws(&text)),
                    b"summary" if parent == Some(b"component") => {
                        summary.offer(locales.score(elem_lang.as_deref()), collapse_ws(&text))
                    }
                    b"p" | b"li" => {
                        let score = locales.score(elem_lang.as_deref());
                        let t = collapse_ws(&text);
                        let is_li = local.as_slice() == b"li";
                        if let Some((_, d)) = &mut release {
                            if is_li { d.item(score, t) } else { d.para(score, t) }
                        } else if stack.iter().any(|s| s.as_slice() == b"description") {
                            if is_li { desc.item(score, t) } else { desc.para(score, t) }
                        }
                        text.clear();
                    }
                    b"ul" | b"ol" => {
                        if let Some((_, d)) = &mut release {
                            d.end_list();
                        } else {
                            desc.end_list();
                        }
                    }
                    b"description" => desc_lang = None,
                    b"icon" if parent == Some(b"component") => {
                        cur_icon.value = text.trim().to_string();
                        icons.push(std::mem::take(&mut cur_icon));
                    }
                    b"category" => {
                        let t = text.trim();
                        if !t.is_empty() && !c.categories.iter().any(|x| x == t) {
                            c.categories.push(t.to_string());
                        }
                    }
                    b"keyword" => {
                        if keyword_ok {
                            let t = text.trim();
                            if !t.is_empty() {
                                c.keywords.push(t.to_lowercase());
                            }
                        }
                    }
                    b"project_license" => c.license = Some(text.trim().to_string()),
                    b"url" => {
                        let t = text.trim();
                        if !t.is_empty() && !c.urls.iter().any(|(k, _)| *k == url_kind) {
                            c.urls.push((url_kind.clone(), t.to_string()));
                        }
                    }
                    b"launchable" => {
                        if launch_kind == "desktop-id" && c.launchable.is_none() {
                            c.launchable = Some(text.trim().to_string());
                        }
                    }
                    b"value" => c.custom.push((custom_key.clone(), text.trim().to_string())),
                    b"kudo" => c.kudos.push(text.trim().to_string()),
                    b"content_attribute" => c.content_rating.push((rating_id.clone(), text.trim().to_string())),
                    b"bundle" => {
                        if bundle_kind == "flatpak" {
                            c.bundle = Some(text.trim().to_string());
                        }
                    }
                    b"image" => {
                        if let Some((_, thumb, full, _)) = &mut shot {
                            let url = text.trim().to_string();
                            if img_kind == "source" {
                                *full = Some(url);
                            } else {
                                // Prefer a thumbnail around 624-752px wide.
                                let better = match thumb {
                                    None => true,
                                    Some((w, _)) => {
                                        let d_new = (img_width as i64 - 700).abs();
                                        let d_old = (*w as i64 - 700).abs();
                                        d_new < d_old
                                    }
                                };
                                if better {
                                    *thumb = Some((img_width, url));
                                }
                            }
                        }
                    }
                    b"caption" => {
                        if let Some((_, _, _, cap)) = &mut shot {
                            cap.offer(locales.score(elem_lang.as_deref()), collapse_ws(&text));
                        }
                    }
                    b"screenshot" => {
                        if let Some((kind, thumb, full, cap)) = shot.take() {
                            let full = full.or_else(|| thumb.as_ref().map(|t| t.1.clone()));
                            if let Some(full) = full {
                                let s = Screenshot {
                                    thumbnail: thumb.map(|t| t.1),
                                    full,
                                    caption: Some(cap.value).filter(|v| !v.is_empty()),
                                };
                                if kind.as_deref() == Some("default") {
                                    c.screenshots.insert(0, s);
                                } else {
                                    c.screenshots.push(s);
                                }
                            }
                        }
                    }
                    b"release" => {
                        if let Some((mut r, d)) = release.take() {
                            r.description = d.finish();
                            c.releases.push(r);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok((origin, out))
}

fn pick_icon(icons: &[IconCandidate], base: Option<&IconBase>, origin: Option<&str>) -> Option<Icon> {
    // Prefer cached icons near 128px, then local files, then stock, then remote.
    let mut cached: Vec<&IconCandidate> = icons.iter().filter(|i| i.kind == "cached").collect();
    cached.sort_by_key(|i| {
        let eff = i.width * i.scale;
        if eff >= 128 { eff - 128 } else { 1000 + (128 - eff) }
    });
    if let Some(base) = base {
        for c in cached {
            let dir = if c.scale > 1 {
                format!("{}x{}@{}", c.width, c.width, c.scale)
            } else {
                format!("{}x{}", c.width, c.width)
            };
            let p = match base {
                IconBase::PerOrigin(b) => b.join(origin.unwrap_or("")).join(&dir).join(&c.value),
                IconBase::Flat(b) => b.join(&dir).join(&c.value),
            };
            if p.exists() {
                return Some(Icon::File(p));
            }
        }
    }
    if let Some(l) = icons.iter().find(|i| i.kind == "local" && Path::new(&i.value).exists()) {
        return Some(Icon::File(PathBuf::from(&l.value)));
    }
    if let Some(s) = icons.iter().find(|i| i.kind == "stock") {
        return Some(Icon::Stock(s.value.clone()));
    }
    icons
        .iter()
        .filter(|i| i.kind == "remote")
        .max_by_key(|i| i.width.min(256))
        .map(|r| Icon::Remote(r.value.clone()))
}

#[allow(dead_code)]
pub fn parse_bytes(data: &[u8], locales: &Locales) -> anyhow::Result<Vec<Component>> {
    let r: Box<dyn Read> = Box::new(data);
    Ok(parse_reader(BufReader::new(r), None, locales)?.1)
}
