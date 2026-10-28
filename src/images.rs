//! Asynchronous icon and screenshot loading with an in-memory texture cache
//! and an on-disk download cache.

use crate::model::Icon;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

const MAX_CACHED: usize = 600;
const WORKERS: usize = 6;

type Job = Box<dyn FnOnce() + Send>;

thread_local! {
    static CACHE: RefCell<(HashMap<String, gdk::Texture>, VecDeque<String>)> = RefCell::new((HashMap::new(), VecDeque::new()));
    static POOL: async_channel::Sender<Job> = start_pool();
    static PENDING: RefCell<HashMap<String, Vec<async_channel::Sender<Option<gdk::Texture>>>>> = RefCell::new(HashMap::new());
}

fn start_pool() -> async_channel::Sender<Job> {
    let (tx, rx) = async_channel::unbounded::<Job>();
    for i in 0..WORKERS {
        let rx = rx.clone();
        std::thread::Builder::new()
            .name(format!("img-{i}"))
            .spawn(move || {
                while let Ok(job) = rx.recv_blocking() {
                    // Never let one bad image kill the worker.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
            })
            .expect("spawn image worker");
    }
    tx
}

fn download_dir() -> PathBuf {
    crate::config::cache_dir().join("images")
}

fn url_cache_path(url: &str) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let ext = url.rsplit('.').next().filter(|e| e.len() <= 4 && !e.contains('/')).unwrap_or("img");
    download_dir().join(format!("{:016x}.{ext}", h.finish()))
}

fn fetch_url(url: &str) -> Option<PathBuf> {
    let path = url_cache_path(url);
    if path.exists() {
        return Some(path);
    }
    let resp = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
        .get(url)
        .call()
        .ok()?;
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(resp.into_reader(), 25 * 1024 * 1024), &mut data).ok()?;
    let _ = std::fs::create_dir_all(download_dir());
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &data).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

fn cache_get(key: &str) -> Option<gdk::Texture> {
    CACHE.with(|c| c.borrow().0.get(key).cloned())
}

fn cache_put(key: String, tex: gdk::Texture) {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.0.insert(key.clone(), tex).is_none() {
            c.1.push_back(key);
            while c.1.len() > MAX_CACHED {
                if let Some(old) = c.1.pop_front() {
                    c.0.remove(&old);
                }
            }
        }
    });
}

/// Load a texture from a local path or URL. Requests for the same key are coalesced.
pub async fn load(source: String, remote: bool) -> Option<gdk::Texture> {
    if let Some(t) = cache_get(&source) {
        return Some(t);
    }
    let (tx, rx) = async_channel::bounded(1);
    let first = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let list = p.entry(source.clone()).or_default();
        list.push(tx);
        list.len() == 1
    });
    if first {
        let (res_tx, res_rx) = async_channel::bounded::<Option<gdk::Texture>>(1);
        let src = source.clone();
        let job: Job = Box::new(move || {
            let path = if remote { fetch_url(&src) } else { Some(PathBuf::from(&src)) };
            let tex = path.and_then(|p| gdk::Texture::from_filename(&p).ok());
            let _ = res_tx.send_blocking(tex);
        });
        POOL.with(|p| {
            let _ = p.send_blocking(job);
        });
        let key = source.clone();
        glib::spawn_future_local(async move {
            let tex = res_rx.recv().await.ok().flatten();
            if let Some(t) = &tex {
                cache_put(key.clone(), t.clone());
            }
            let waiters = PENDING.with(|p| p.borrow_mut().remove(&key)).unwrap_or_default();
            for w in waiters {
                let _ = w.try_send(tex.clone());
            }
        });
    }
    rx.recv().await.ok().flatten()
}

const KEY: &str = "jh-image-key";

fn set_key(image: &impl IsA<glib::Object>, key: &str) {
    unsafe { image.set_data::<String>(KEY, key.to_string()) };
}

fn key_matches(image: &impl IsA<glib::Object>, key: &str) -> bool {
    unsafe { image.data::<String>(KEY).map(|p| p.as_ref() == key).unwrap_or(false) }
}

/// Show an app icon in `image`, falling back to a generic icon.
pub fn set_icon(image: &gtk::Image, icon: Option<&Icon>, pixel_size: i32) {
    image.set_pixel_size(pixel_size);
    let fallback = "application-x-executable";
    match icon {
        None => {
            set_key(image, "");
            image.set_icon_name(Some(fallback));
        }
        Some(Icon::Stock(name)) => {
            set_key(image, name);
            let theme = gtk::IconTheme::for_display(&image.display());
            if theme.has_icon(name) {
                image.set_icon_name(Some(name));
            } else {
                image.set_icon_name(Some(fallback));
            }
        }
        Some(Icon::File(p)) => {
            let key = p.to_string_lossy().into_owned();
            load_into(image, key, false, fallback);
        }
        Some(Icon::Remote(url)) => load_into(image, url.clone(), true, fallback),
    }
}

fn load_into(image: &gtk::Image, key: String, remote: bool, fallback: &str) {
    set_key(image, &key);
    if let Some(t) = cache_get(&key) {
        image.set_paintable(Some(&t));
        return;
    }
    image.set_icon_name(Some(fallback));
    let weak = image.downgrade();
    glib::spawn_future_local(async move {
        let tex = load(key.clone(), remote).await;
        if let (Some(img), Some(tex)) = (weak.upgrade(), tex)
            && key_matches(&img, &key) {
                img.set_paintable(Some(&tex));
            }
    });
}

/// Load a screenshot into a `gtk::Picture`.
pub fn set_picture(picture: &gtk::Picture, url: &str, on_done: impl FnOnce(bool) + 'static) {
    let key = url.to_string();
    set_key(picture, &key);
    if let Some(t) = cache_get(&key) {
        picture.set_paintable(Some(&t));
        on_done(true);
        return;
    }
    let remote = url.starts_with("http");
    let weak = picture.downgrade();
    glib::spawn_future_local(async move {
        let tex = load(key.clone(), remote).await;
        let Some(p) = weak.upgrade() else { return };
        if key_matches(&p, &key) {
            if let Some(t) = &tex {
                p.set_paintable(Some(t));
            }
            on_done(tex.is_some());
        }
    });
}

/// Delete downloaded images older than 30 days.
pub fn prune_disk_cache() {
    std::thread::spawn(|| {
        let Ok(rd) = std::fs::read_dir(download_dir()) else { return };
        let limit = std::time::SystemTime::now() - Duration::from_secs(30 * 86400);
        for e in rd.flatten() {
            if let Ok(m) = e.metadata()
                && m.modified().map(|t| t < limit).unwrap_or(false) {
                    let _ = std::fs::remove_file(e.path());
                }
        }
    });
}
