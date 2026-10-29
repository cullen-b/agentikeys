//! `agentikeys setup`: teaches every AI coding agent on this machine to use agentikeys, by adding a
//! short instruction block to each installed harness's user-level instructions file.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const START: &str =
    "<!-- agentikeys:start (managed by `agentikeys setup`; `agentikeys setup --uninstall` removes it) -->";
const END: &str = "<!-- agentikeys:end -->";

/// What agents are told. Kept short: some harnesses cap global rules (Windsurf: 6,000 characters).
pub const INSTRUCTIONS: &str = r#"## Secrets: use agentikeys

Never ask the user to paste an API key, token, password or other secret into the chat, and never put
one on a command line. When a task needs a secret, use the `agentikeys` CLI so the value never passes
through you:

1. Run `agentikeys request` with a JSON spec on stdin: a short `title`, a `reason`, the `fields`
   (`name`, plus optional `label`, `help_url`, `placeholder`, `pattern`, `optional`), and a suggested
   `destination`: the file the app reads, and/or a secret store the project uses. For example:
   `{"title": "Stripe test keys", "reason": "Checkout needs them.", "fields": [{"name": "STRIPE_SECRET_KEY", "help_url": "https://dashboard.stripe.com/test/apikeys"}], "destination": {"path": ".env.local"}}`
   A store suggestion looks like `"destination": {"store": {"type": "gcp_secret_manager", "project": "my-project"}}`
   (also `aws_secrets_manager`, `aws_parameter_store`, `azure_key_vault` with `"vault"`).
2. Post the returned `url` in the chat as a link for the user to open in their own browser, with any
   `note`. Never open the link yourself, including in built-in or automated browsers. The user picks
   where the values are saved and may paste multi-line keys or upload key files.
3. Run `agentikeys wait <id>` until `status` isn't `pending` (exit code 4 means still pending; run it
   again).
4. Use the receipt's `saved_to` (file paths, store `references`) to wire up configuration. Never read,
   print or log the secret values, and don't open the files they were saved to.

If the user isn't at this machine, add `--via tailscale` (or `--via tunnel` without Tailscale). Check
whether a key was set up before with `agentikeys log --key NAME --json`. `agentikeys --help` has more."#;

/// A harness and where it reads user-level instructions.
struct Target {
    name: &'static str,
    /// Its config directory; the harness counts as installed when this exists.
    detect: &'static str,
    file: &'static str,
    /// The whole file is ours (a rules directory), rather than a block inside a shared file.
    dedicated: bool,
}

const TARGETS: &[Target] = &[
    Target { name: "Claude Code", detect: ".claude", file: ".claude/CLAUDE.md", dedicated: false },
    Target { name: "Codex", detect: ".codex", file: ".codex/AGENTS.md", dedicated: false },
    Target { name: "OpenCode", detect: ".config/opencode", file: ".config/opencode/AGENTS.md", dedicated: false },
    Target { name: "Gemini CLI", detect: ".gemini", file: ".gemini/GEMINI.md", dedicated: false },
    Target { name: "GitHub Copilot", detect: ".copilot", file: ".copilot/copilot-instructions.md", dedicated: false },
    Target { name: "Amp", detect: ".config/amp", file: ".config/amp/AGENTS.md", dedicated: false },
    Target { name: "Zed", detect: ".config/zed", file: ".config/zed/AGENTS.md", dedicated: false },
    Target { name: "Devin", detect: ".config/devin", file: ".config/devin/AGENTS.md", dedicated: false },
    Target { name: "Goose", detect: ".config/goose", file: ".config/goose/.goosehints", dedicated: false },
    Target {
        name: "Windsurf",
        detect: ".codeium/windsurf",
        file: ".codeium/windsurf/memories/global_rules.md",
        dedicated: false,
    },
    Target { name: "Cline", detect: "Documents/Cline", file: "Documents/Cline/Rules/agentikeys.md", dedicated: true },
    Target { name: "Roo Code", detect: ".roo", file: ".roo/rules/agentikeys.md", dedicated: true },
    Target { name: "Kiro", detect: ".kiro", file: ".kiro/steering/agentikeys.md", dedicated: true },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Uninstall,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Updated,
    Unchanged,
    Removed,
    NotPresent,
}

pub struct Report {
    pub harness: &'static str,
    pub file: PathBuf,
    pub change: Change,
}

/// Installs or removes the instructions for every harness found under `home`. With `dry_run`, only reports.
pub fn run(home: &Path, action: Action, dry_run: bool) -> Result<Vec<Report>> {
    let mut reports = Vec::new();
    for t in TARGETS.iter().filter(|t| home.join(t.detect).is_dir()) {
        let file = home.join(t.file);
        let existing = match fs::read_to_string(&file) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        };
        let (next, change) = match (action, t.dedicated) {
            (Action::Install, true) => {
                let next = format!("{INSTRUCTIONS}\n");
                let change = match &existing {
                    None => Change::Added,
                    Some(e) if *e == next => Change::Unchanged,
                    Some(_) => Change::Updated,
                };
                (Some(next), change)
            }
            (Action::Uninstall, true) => match existing {
                Some(_) => (None, Change::Removed),
                None => (None, Change::NotPresent),
            },
            (Action::Install, false) => {
                let current = existing.clone().unwrap_or_default();
                let next = with_block(&current);
                let change = if next == current {
                    Change::Unchanged
                } else if current.contains(START) {
                    Change::Updated
                } else {
                    Change::Added
                };
                (Some(next), change)
            }
            (Action::Uninstall, false) => {
                let current = existing.clone().unwrap_or_default();
                let next = without_block(&current);
                if next == current { (Some(next), Change::NotPresent) } else { (Some(next), Change::Removed) }
            }
        };
        if !dry_run && !matches!(change, Change::Unchanged | Change::NotPresent) {
            match next {
                Some(text) => {
                    fs::create_dir_all(file.parent().expect("has parent"))?;
                    fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
                }
                None => fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?,
            }
        }
        reports.push(Report { harness: t.name, file, change });
    }
    Ok(reports)
}

/// `text` with our block added at the end, or replaced in place if it's already there.
fn with_block(text: &str) -> String {
    let block = format!("{START}\n{INSTRUCTIONS}\n{END}\n");
    if let (Some(s), Some(e)) = (text.find(START), text.find(END))
        && s < e
    {
        let after = text[e + END.len()..].strip_prefix('\n').unwrap_or(&text[e + END.len()..]);
        return format!("{}{block}{after}", &text[..s]);
    }
    let sep = match text {
        "" => "",
        t if t.ends_with("\n\n") => "",
        t if t.ends_with('\n') => "\n",
        _ => "\n\n",
    };
    format!("{text}{sep}{block}")
}

fn without_block(text: &str) -> String {
    match (text.find(START), text.find(END)) {
        (Some(s), Some(e)) if s < e => {
            let before = text[..s].trim_end_matches('\n');
            let after = text[e + END.len()..].trim_start_matches('\n');
            match (before.is_empty(), after.is_empty()) {
                (true, _) => after.to_string(),
                (false, true) => format!("{before}\n"),
                (false, false) => format!("{before}\n\n{after}"),
            }
        }
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str, dirs: &[&str]) -> PathBuf {
        let h = std::env::temp_dir().join(format!("agentikeys-setup-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&h);
        for d in dirs {
            fs::create_dir_all(h.join(d)).unwrap();
        }
        h
    }

    #[test]
    fn installs_only_where_harnesses_exist_and_is_idempotent() {
        let h = home("install", &[".claude", ".kiro"]);
        fs::write(h.join(".claude/CLAUDE.md"), "# My rules\nBe nice.\n").unwrap();
        let first = run(&h, Action::Install, false).unwrap();
        assert_eq!(first.iter().map(|r| r.harness).collect::<Vec<_>>(), ["Claude Code", "Kiro"]);
        assert!(first.iter().all(|r| r.change == Change::Added));
        let claude = fs::read_to_string(h.join(".claude/CLAUDE.md")).unwrap();
        assert!(claude.starts_with("# My rules\nBe nice.\n\n<!-- agentikeys:start"));
        assert!(claude.contains("agentikeys request") && claude.ends_with("<!-- agentikeys:end -->\n"));
        assert!(h.join(".kiro/steering/agentikeys.md").exists());
        assert!(!h.join(".codex").exists(), "never creates a harness's config dir");

        let again = run(&h, Action::Install, false).unwrap();
        assert!(again.iter().all(|r| r.change == Change::Unchanged));
        assert_eq!(fs::read_to_string(h.join(".claude/CLAUDE.md")).unwrap(), claude);
    }

    #[test]
    fn updates_an_old_block_in_place_and_uninstalls_cleanly() {
        let h = home("update", &[".codex"]);
        let file = h.join(".codex/AGENTS.md");
        fs::write(&file, format!("intro\n\n{START}\nold text\n{END}\n\nmore rules\n")).unwrap();
        assert_eq!(run(&h, Action::Install, false).unwrap()[0].change, Change::Updated);
        let text = fs::read_to_string(&file).unwrap();
        assert!(
            text.starts_with("intro\n\n<!-- agentikeys:start")
                && text.ends_with("<!-- agentikeys:end -->\n\nmore rules\n")
        );
        assert!(!text.contains("old text"));

        assert_eq!(run(&h, Action::Uninstall, false).unwrap()[0].change, Change::Removed);
        assert_eq!(fs::read_to_string(&file).unwrap(), "intro\n\nmore rules\n");
        assert_eq!(run(&h, Action::Uninstall, false).unwrap()[0].change, Change::NotPresent);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let h = home("dry", &[".gemini"]);
        assert_eq!(run(&h, Action::Install, true).unwrap()[0].change, Change::Added);
        assert!(!h.join(".gemini/GEMINI.md").exists());
    }

    #[test]
    fn fits_windsurf_limit() {
        assert!(with_block("").len() < 6000);
    }
}
