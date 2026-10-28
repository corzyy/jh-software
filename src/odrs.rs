//! Open Desktop Ratings Service client (the reviews used by GNOME Software).

use serde::Deserialize;
use std::time::Duration;

const BASE: &str = "https://odrs.gnome.org/1.0/reviews/api";

#[derive(Debug, Clone, Default)]
pub struct Ratings {
    /// Counts for 1..=5 stars.
    pub stars: [u64; 5],
    pub total: u64,
}

impl Ratings {
    pub fn average(&self) -> f64 {
        let n: u64 = self.stars.iter().sum();
        if n == 0 {
            return 0.0;
        }
        self.stars.iter().enumerate().map(|(i, c)| (i as u64 + 1) * c).sum::<u64>() as f64 / n as f64
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub rating: u32,
    #[serde(default)]
    pub reviewer_name: Option<String>,
    #[serde(default)]
    pub date_created: f64,
    #[serde(default)]
    pub version: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .user_agent(&format!("jh-software/{}", crate::config::VERSION))
        .build()
}

fn candidate_ids(id: &str) -> Vec<String> {
    let base = id.strip_suffix(".desktop").unwrap_or(id);
    vec![format!("{base}.desktop"), base.to_string()]
}

pub fn ratings(id: &str) -> Option<Ratings> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        star1: u64,
        #[serde(default)]
        star2: u64,
        #[serde(default)]
        star3: u64,
        #[serde(default)]
        star4: u64,
        #[serde(default)]
        star5: u64,
        #[serde(default)]
        total: u64,
    }
    let a = agent();
    let mut best: Option<Ratings> = None;
    for cid in candidate_ids(id) {
        let Ok(resp) = a.get(&format!("{BASE}/ratings/{cid}")).call() else { continue };
        let Ok(r) = resp.into_json::<Raw>() else { continue };
        let rt = Ratings { stars: [r.star1, r.star2, r.star3, r.star4, r.star5], total: r.total };
        if rt.total > best.as_ref().map(|b| b.total).unwrap_or(0) {
            best = Some(rt);
        }
    }
    best
}

fn user_hash() -> String {
    // ODRS wants a stable, anonymous 40 char hex id.
    use std::hash::{Hash, Hasher};
    let machine = std::fs::read_to_string("/etc/machine-id").unwrap_or_default();
    let user = gtk::glib::user_name().to_string_lossy().into_owned();
    let mut out = String::new();
    for salt in 0..3u8 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (salt, &machine, &user, "jh-software").hash(&mut h);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(40);
    out
}

pub fn reviews(id: &str, version: Option<&str>) -> Vec<Review> {
    let ids = candidate_ids(id);
    let locale = std::env::var("LANG").ok().and_then(|l| l.split('.').next().map(String::from)).unwrap_or_else(|| "en_US".into());
    let body = serde_json::json!({
        "user_hash": user_hash(),
        "app_id": ids[0],
        "compat_ids": ids,
        "locale": locale,
        "distro": "Fedora",
        "version": version.unwrap_or("unknown"),
        "limit": 30,
    });
    let Ok(resp) = agent().post(&format!("{BASE}/fetch")).send_json(body) else { return Vec::new() };
    let mut v: Vec<Review> = resp.into_json().unwrap_or_default();
    v.retain(|r| !r.description.trim().is_empty());
    v
}
