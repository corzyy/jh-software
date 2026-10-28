//! Fast installed-package queries straight from the RPM database.

use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct InstalledRpm {
    pub evr: String,
    pub size: u64,
    pub vendor: String,
}

impl InstalledRpm {
    /// Terra signs its packages with the "Terra" vendor tag.
    pub fn is_terra(&self) -> bool {
        self.vendor.to_lowercase().contains("terra") || self.vendor.contains("Fyra")
    }
}

/// All installed packages, keyed by name.
pub fn installed() -> HashMap<String, InstalledRpm> {
    let out = Command::new("rpm")
        .args(["-qa", "--nosignature", "--nodigest", "--qf", "%{NAME}\t%{EVR}\t%{SIZE}\t%{VENDOR}\n"])
        .output();
    let mut map = HashMap::with_capacity(4096);
    let Ok(out) = out else { return map };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut f = line.split('\t');
        let (Some(name), Some(evr)) = (f.next(), f.next()) else { continue };
        let size = f.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let vendor = f.next().filter(|r| *r != "(none)").unwrap_or("").to_string();
        if name == "gpg-pubkey" {
            continue;
        }
        map.insert(name.to_string(), InstalledRpm { evr: evr.to_string(), size, vendor });
    }
    map
}

/// Read name/version/summary from a local .rpm file.
pub fn query_file(path: &str) -> Option<(String, String, String, String)> {
    let out = Command::new("rpm")
        .args(["-qp", "--nosignature", "--nodigest", "--qf", "%{NAME}\t%{EVR}\t%{SUMMARY}\t%{DESCRIPTION}", path])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut f = s.splitn(4, '\t');
    Some((
        f.next()?.to_string(),
        f.next()?.to_string(),
        f.next().unwrap_or("").to_string(),
        f.next().unwrap_or("").to_string(),
    ))
}
