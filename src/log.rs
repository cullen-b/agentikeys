//! A local record of requests: what was asked for, by which project, and where it was saved. Never values.

use crate::save::SavedTo;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    pub time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    pub cwd: String,
    /// Field names asked for.
    pub keys: Vec<String>,
    pub via: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saved_to: Vec<SavedTo>,
}

fn path() -> Result<PathBuf> {
    Ok(crate::config_dir()?.join("log.jsonl"))
}

pub fn append(entry: &Entry) -> Result<()> {
    let path = path()?;
    fs::create_dir_all(path.parent().expect("has parent"))?;
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&path).with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

/// The last `n` entries, oldest first, optionally only those that asked for `key`.
pub fn read(n: usize, key: Option<&str>) -> Result<Vec<Entry>> {
    let text = match fs::read_to_string(path()?) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut entries: Vec<Entry> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .filter(|e| key.is_none_or(|k| e.keys.iter().any(|x| x == k)))
        .collect();
    let skip = entries.len().saturating_sub(n);
    entries.drain(..skip);
    Ok(entries)
}

/// One line for people: when, outcome, keys, where they went, and which request asked.
pub fn describe(e: &Entry) -> String {
    let places: Vec<String> =
        e.saved_to.iter().map(|s| format!("{} {}", s.kind.replace('_', " "), s.location)).collect();
    let to = if places.is_empty() { String::new() } else { format!(" -> {}", places.join(", ")) };
    format!("{}  {:<9} {}{}  ({}, in {})", e.time, e.status, e.keys.join(", "), to, e.title, e.cwd)
}

/// UTC timestamp like 2026-10-29T22:51:07Z.
pub fn now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

// Howard Hinnant's days-to-civil-date algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_390), (2025, 10, 29));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }
}
