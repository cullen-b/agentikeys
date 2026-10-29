//! Writes values into a file and keeps it out of git.

use crate::spec::{Format, Spec};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitStatus {
    NotInRepo,
    AlreadyIgnored,
    /// Not ignored yet; `commit` will append it to the repo's .gitignore.
    WillIgnore,
}

/// The suggested file, checked before the user is asked anything so a problem can't waste their input.
pub struct FilePlan {
    pub path: PathBuf,
    pub format: Format,
    pub git: GitStatus,
    repo_root: Option<PathBuf>,
    existing: String,
}

impl Drop for FilePlan {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.existing);
    }
}

pub fn plan_file(spec: &Spec, cwd: &Path) -> Result<Option<FilePlan>> {
    match (&spec.destination.path, spec.file_format()?) {
        (Some(path), Some(format)) => Ok(Some(plan_path(path, format, cwd)?)),
        _ => Ok(None),
    }
}

/// Writes `values` (field name -> value) into the file.
pub fn write_values(spec: &Spec, file: &FilePlan, values: &BTreeMap<String, String>) -> Result<()> {
    let pairs: Vec<(&str, &str)> =
        spec.fields.iter().filter_map(|f| values.get(&f.name).map(|v| (f.key_path(), v.as_str()))).collect();
    write_file(file, &pairs)
}

/// Refuses files tracked by git; untracked ones get gitignored when written.
fn plan_path(path: &Path, format: Format, cwd: &Path) -> Result<FilePlan> {
    let path = absolute(&cwd.join(path))?;
    if path.is_dir() {
        bail!("destination {} is a directory", path.display());
    }
    let existing = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    // Parse now so a malformed file fails before the prompt opens.
    apply(format, &existing, &[]).with_context(|| format!("{} isn't valid {}", path.display(), format.as_str()))?;

    let (repo_root, git) = git_status(&path)?;
    Ok(FilePlan { path, format, git, repo_root, existing })
}

/// Writes `pairs` (key path -> value) to the file, ignoring it in git first if needed.
fn write_file(plan: &FilePlan, pairs: &[(&str, &str)]) -> Result<()> {
    if plan.git == GitStatus::WillIgnore {
        let root = plan.repo_root.as_ref().expect("WillIgnore implies a repo");
        add_to_gitignore(root, &plan.path)?;
    }
    let out = zeroize::Zeroizing::new(apply(plan.format, &plan.existing, pairs)?);
    if let Some(dir) = plan.path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    write_private(&plan.path, &out)
}

fn apply(format: Format, existing: &str, pairs: &[(&str, &str)]) -> Result<String> {
    match format {
        Format::Dotenv => Ok(dotenv::upsert(existing, pairs)),
        Format::Json => json_upsert(existing, pairs),
        Format::Toml => toml_upsert(existing, pairs),
    }
}

fn json_upsert(existing: &str, pairs: &[(&str, &str)]) -> Result<String> {
    use serde_json::{Map, Value};
    let mut root: Value =
        if existing.trim().is_empty() { Value::Object(Map::new()) } else { serde_json::from_str(existing)? };
    if !root.is_object() {
        bail!("top level is not an object");
    }
    for (path, val) in pairs {
        let mut node = &mut root;
        let segs: Vec<&str> = path.split('.').collect();
        for seg in &segs[..segs.len() - 1] {
            let obj = node.as_object_mut().ok_or_else(|| anyhow!("{path}: {seg} is not an object"))?;
            node = obj.entry(*seg).or_insert_with(|| Value::Object(Map::new()));
        }
        let obj = node.as_object_mut().ok_or_else(|| anyhow!("{path}: parent is not an object"))?;
        obj.insert(segs[segs.len() - 1].to_string(), Value::String(val.to_string()));
    }
    Ok(serde_json::to_string_pretty(&root)? + "\n")
}

fn toml_upsert(existing: &str, pairs: &[(&str, &str)]) -> Result<String> {
    use toml_edit::{DocumentMut, Item, Table, value};
    let mut doc: DocumentMut = existing.parse()?;
    for (path, val) in pairs {
        let segs: Vec<&str> = path.split('.').collect();
        let mut table = doc.as_table_mut();
        for seg in &segs[..segs.len() - 1] {
            let item = table.entry(seg).or_insert_with(|| Item::Table(Table::new()));
            table = item.as_table_mut().ok_or_else(|| anyhow!("{path}: {seg} is not a table"))?;
        }
        table[segs[segs.len() - 1]] = value(*val);
    }
    Ok(doc.to_string())
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("secrets");
    let tmp = path.with_file_name(format!(".{name}.agentikeys-{}.tmp", std::process::id()));
    let result = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("writing {}", path.display()))
}

fn git_status(path: &Path) -> Result<(Option<PathBuf>, GitStatus)> {
    let dir = path.parent().ok_or_else(|| anyhow!("destination has no parent directory"))?;
    let probe = nearest_existing(dir);
    let Some(root) = git(&probe, &["rev-parse", "--show-toplevel"]).map(PathBuf::from) else {
        return Ok((None, GitStatus::NotInRepo));
    };
    let rel = path.strip_prefix(&root).unwrap_or(path).to_string_lossy().to_string();
    if git(&root, &["ls-files", "--error-unmatch", "--", &rel]).is_some() {
        bail!(
            "{rel} is tracked by git; refusing to write secrets into a committed file. Pick an untracked file such as .env.local"
        );
    }
    let status = if git(&root, &["check-ignore", "-q", "--", &rel]).is_some() {
        GitStatus::AlreadyIgnored
    } else {
        GitStatus::WillIgnore
    };
    Ok((Some(root), status))
}

fn add_to_gitignore(root: &Path, path: &Path) -> Result<()> {
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    let gi = root.join(".gitignore");
    let mut existing = fs::read_to_string(&gi).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == format!("/{rel}")) {
        return Ok(());
    }
    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(&format!("/{rel}\n"));
    fs::write(&gi, existing).with_context(|| format!("updating {}", gi.display()))
}

/// Runs git and returns trimmed stdout on success.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn nearest_existing(dir: &Path) -> PathBuf {
    dir.ancestors().find(|d| d.is_dir()).unwrap_or(Path::new("/")).to_path_buf()
}

/// Absolute path with symlinks in existing ancestors resolved, so git paths line up.
fn absolute(path: &Path) -> Result<PathBuf> {
    if let Ok(p) = path.canonicalize() {
        return Ok(p);
    }
    let parent = path.parent().ok_or_else(|| anyhow!("bad destination path"))?;
    let name = path.file_name().ok_or_else(|| anyhow!("destination has no file name"))?;
    Ok(absolute(parent).unwrap_or_else(|_| parent.to_path_buf()).join(name))
}

mod dotenv {
    pub fn upsert(existing: &str, pairs: &[(&str, &str)]) -> String {
        let mut lines: Vec<String> = existing.lines().map(String::from).collect();
        for (key, val) in pairs {
            let rendered = format!("{key}={}", quote(val));
            match lines.iter().position(|l| defines(l, key)) {
                Some(i) => {
                    let export = if lines[i].trim_start().starts_with("export ") { "export " } else { "" };
                    lines[i] = format!("{export}{rendered}");
                }
                None => lines.push(rendered),
            }
        }
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    fn defines(line: &str, key: &str) -> bool {
        let l = line.trim_start();
        let l = l.strip_prefix("export ").map(str::trim_start).unwrap_or(l);
        l.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='))
    }

    fn quote(val: &str) -> String {
        let plain = |c: char| c.is_ascii_alphanumeric() || "_-./+=:@,".contains(c);
        if val.chars().all(plain) {
            val.to_string()
        } else if !val.contains('\'') && !val.contains('\n') {
            format!("'{val}'")
        } else {
            let escaped = val.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$").replace('\n', "\\n");
            format!("\"{escaped}\"")
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn replaces_in_place_and_appends() {
            let src = "# comment\nexport A=old\nB = 2\n";
            let out = upsert(src, &[("A", "new"), ("C", "has space")]);
            assert_eq!(out, "# comment\nexport A=new\nB = 2\nC='has space'\n");
        }

        #[test]
        fn quotes_awkward_values() {
            assert_eq!(quote("sk_test_abc123"), "sk_test_abc123");
            assert_eq!(quote("a$b"), "'a$b'");
            assert_eq!(quote("it's"), "\"it's\"");
            assert_eq!(quote("x\"$\ny"), "\"x\\\"\\$\\ny\"");
        }

        #[test]
        fn prefix_keys_do_not_match() {
            let out = upsert("API_KEY_OLD=1\n", &[("API_KEY", "2")]);
            assert_eq!(out, "API_KEY_OLD=1\nAPI_KEY=2\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_nested_upsert_keeps_other_keys() {
        let out = json_upsert(r#"{"name":"app","stripe":{"mode":"test"}}"#, &[("stripe.secret", "sk")]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["name"], "app");
        assert_eq!(v["stripe"]["mode"], "test");
        assert_eq!(v["stripe"]["secret"], "sk");
    }

    #[test]
    fn toml_upsert_keeps_comments() {
        let out = toml_upsert("# keep me\n[db]\nhost = \"x\"\n", &[("db.password", "p"), ("token", "t")]).unwrap();
        assert!(out.contains("# keep me"));
        assert!(out.contains("password = \"p\""));
        assert!(out.contains("token = \"t\""));
    }
}
