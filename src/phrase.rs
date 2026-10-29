//! The user's security phrase: shown on every real agentikeys page and never given to agents, so a
//! lookalike page hosted elsewhere can't show it.

use anyhow::{Context, Result, anyhow};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const ADJECTIVES: [&str; 64] = [
    "amber", "brave", "calm", "clever", "coral", "crisp", "dusty", "eager", "early", "fancy", "gentle", "glad",
    "golden", "grand", "happy", "hidden", "humble", "jolly", "kind", "lively", "lucky", "mellow", "misty", "noble",
    "olive", "proud", "quick", "quiet", "rapid", "rosy", "royal", "rustic", "sandy", "scarlet", "shiny", "silent",
    "silver", "sleepy", "smooth", "snowy", "solar", "spicy", "steady", "sunny", "swift", "tidy", "tiny", "true",
    "velvet", "vivid", "warm", "wild", "windy", "wise", "witty", "young", "zesty", "azure", "bold", "breezy", "cosmic",
    "dapper", "fuzzy", "lunar",
];

const NOUNS: [&str; 64] = [
    "otter", "falcon", "maple", "harbor", "comet", "cedar", "badger", "canyon", "dolphin", "ember", "fern", "glacier",
    "heron", "island", "jaguar", "kettle", "lantern", "meadow", "nectar", "orchid", "panda", "quartz", "raven",
    "river", "saddle", "tiger", "tulip", "valley", "walrus", "willow", "yak", "zebra", "acorn", "beacon", "bison",
    "cactus", "dune", "eagle", "fjord", "gecko", "hazel", "iris", "koala", "lagoon", "lynx", "magnet", "nebula", "oak",
    "pebble", "puffin", "reef", "sparrow", "summit", "thistle", "tundra", "violet", "wren", "anchor", "basil",
    "cobalt", "delta", "garnet", "marble", "pine",
];

fn path() -> Result<PathBuf> {
    Ok(crate::config_dir()?.join("phrase"))
}

/// The phrase, creating one on first use. The bool is true when it was just created.
pub fn get_or_create() -> Result<(String, bool)> {
    let path = path()?;
    match fs::read_to_string(&path) {
        Ok(p) if !p.trim().is_empty() => Ok((p.trim().to_string(), false)),
        _ => Ok((create(&path)?, true)),
    }
}

pub fn reset() -> Result<String> {
    create(&path()?)
}

fn create(path: &Path) -> Result<String> {
    let mut bytes = [0u8; 3];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("no randomness: {e}"))?;
    let phrase = format!(
        "{}-{}-{}",
        ADJECTIVES[bytes[0] as usize % 64],
        NOUNS[bytes[1] as usize % 64],
        10 + bytes[2] as usize % 90
    );
    fs::create_dir_all(path.parent().expect("has parent"))?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(path).with_context(|| format!("writing {}", path.display()))?;
    f.write_all(phrase.as_bytes())?;
    Ok(phrase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_look_right_and_vary() {
        let dir = std::env::temp_dir().join(format!("agentikeys-phrase-{}", std::process::id()));
        let path = dir.join("phrase");
        let a = create(&path).unwrap();
        let parts: Vec<&str> = a.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!(ADJECTIVES.contains(&parts[0]) && NOUNS.contains(&parts[1]));
        assert!((10..100).contains(&parts[2].parse::<u32>().unwrap()));
        assert_eq!(fs::read_to_string(&path).unwrap(), a);
        let distinct: std::collections::HashSet<String> = (0..20).map(|_| create(&path).unwrap()).collect();
        assert!(distinct.len() > 1);
        let _ = fs::remove_dir_all(dir);
    }
}
