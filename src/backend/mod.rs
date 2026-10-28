//! Package management backends.
//!
//! Every function in here is blocking and must be called off the main thread.

pub mod flatpak;
pub mod packagekit;
pub mod rpm;
pub mod terra;

/// Progress reporting from a running backend operation.
pub trait Progress: Send + Sync {
    /// `fraction` in 0.0..=1.0, or `None` when unknown.
    fn update(&self, fraction: Option<f64>, status: &str);
    fn cancelled(&self) -> bool {
        false
    }
}

pub struct NoProgress;
impl Progress for NoProgress {
    fn update(&self, _: Option<f64>, _: &str) {}
}

/// Compare two RPM version strings (simplified rpmvercmp).
pub fn rpm_vercmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        // Skip separators, but treat '~' (pre-release) specially.
        while !a.is_empty() && !a[0].is_ascii_alphanumeric() && a[0] != b'~' {
            a = &a[1..];
        }
        while !b.is_empty() && !b[0].is_ascii_alphanumeric() && b[0] != b'~' {
            b = &b[1..];
        }
        match (a.first(), b.first()) {
            (Some(b'~'), Some(b'~')) => {
                a = &a[1..];
                b = &b[1..];
                continue;
            }
            (Some(b'~'), _) => return Less,
            (_, Some(b'~')) => return Greater,
            (None, None) => return Equal,
            (None, _) => return Less,
            (_, None) => return Greater,
            _ => {}
        }
        let numeric = a[0].is_ascii_digit();
        let take = |s: &[u8]| -> usize {
            s.iter()
                .take_while(|c| if numeric { c.is_ascii_digit() } else { c.is_ascii_alphabetic() })
                .count()
        };
        let (la, lb) = (take(a), take(b));
        if lb == 0 {
            // Different segment types: numeric is newer.
            return if numeric { Greater } else { Less };
        }
        let (sa, sb) = (&a[..la], &b[..lb]);
        let ord = if numeric {
            let ta = trim_zeros(sa);
            let tb = trim_zeros(sb);
            ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb))
        } else {
            sa.cmp(sb)
        };
        if ord != Equal {
            return ord;
        }
        a = &a[la..];
        b = &b[lb..];
    }
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|c| **c == b'0').count();
    &s[n..]
}

#[cfg(test)]
mod tests {
    use super::rpm_vercmp;
    use std::cmp::Ordering::*;

    #[test]
    fn vercmp() {
        assert_eq!(rpm_vercmp("1.0", "1.0"), Equal);
        assert_eq!(rpm_vercmp("1.10", "1.9"), Greater);
        assert_eq!(rpm_vercmp("1.0~rc1", "1.0"), Less);
        assert_eq!(rpm_vercmp("2.0-1.fc45", "2.0-2.fc45"), Less);
        assert_eq!(rpm_vercmp("1.0a", "1.0"), Greater);
    }
}
