//! Persistent user preferences (`~/.config/jh-software/config.json`).

use crate::model::SourceKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const APP_ID: &str = "io.github.corzyy.JHSoftware";
pub const APP_NAME: &str = "JH Software";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Preferred install sources, best first.
    pub source_priority: Vec<SourceKind>,
    /// Install flatpaks into the per-user installation instead of system-wide.
    pub flatpak_user: bool,
    /// Show plain packages without AppStream metadata (mostly Terra) in search.
    pub show_packages: bool,
    /// Show proprietary software in the explore page.
    pub show_nonfree: bool,
    /// Check for updates automatically and notify.
    pub auto_check_updates: bool,
    /// Automatically update flatpaks in the background.
    pub auto_update_flatpaks: bool,
    /// Keep running in the background (and autostart) for update notifications.
    pub run_in_background: bool,
    /// Unix timestamps.
    pub last_refresh: u64,
    pub last_update_check: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            source_priority: vec![SourceKind::Rpm, SourceKind::Terra, SourceKind::Flatpak],
            flatpak_user: false,
            show_packages: true,
            show_nonfree: true,
            auto_check_updates: true,
            auto_update_flatpaks: false,
            run_in_background: false,
            last_refresh: 0,
            last_update_check: 0,
        }
    }
}

pub fn config_dir() -> PathBuf {
    gtk::glib::user_config_dir().join("jh-software")
}

pub fn cache_dir() -> PathBuf {
    gtk::glib::user_cache_dir().join("jh-software")
}

impl Config {
    fn path() -> PathBuf {
        config_dir().join("config.json")
    }

    pub fn load() -> Self {
        let mut c: Config = std::fs::read(Self::path())
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default();
        // Make sure every source kind is present exactly once.
        let mut seen = Vec::new();
        c.source_priority.retain(|k| {
            if seen.contains(k) {
                false
            } else {
                seen.push(*k);
                true
            }
        });
        for k in [SourceKind::Rpm, SourceKind::Terra, SourceKind::Flatpak] {
            if !c.source_priority.contains(&k) {
                c.source_priority.push(k);
            }
        }
        c
    }

    pub fn save(&self) {
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(data) = serde_json::to_vec_pretty(self) {
            // Write atomically so a crash never leaves a truncated config.
            let tmp = dir.join("config.json.tmp");
            if std::fs::write(&tmp, data).is_ok() {
                let _ = std::fs::rename(tmp, Self::path());
            }
        }
    }

    pub fn priority_of(&self, kind: SourceKind) -> usize {
        self.source_priority.iter().position(|k| *k == kind).unwrap_or(99)
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write or remove the autostart entry used for background update checks.
pub fn set_autostart(enabled: bool) {
    let dir = gtk::glib::user_config_dir().join("autostart");
    let path = dir.join(format!("{APP_ID}.desktop"));
    if enabled {
        let _ = std::fs::create_dir_all(&dir);
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_else(|| "jh-software".into());
        let content = format!(
            "[Desktop Entry]\nType=Application\nName={APP_NAME}\nExec={exe} --background\nIcon={APP_ID}\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\nX-GNOME-Autostart-Delay=30\n"
        );
        let _ = std::fs::write(path, content);
    } else {
        let _ = std::fs::remove_file(path);
    }
}
