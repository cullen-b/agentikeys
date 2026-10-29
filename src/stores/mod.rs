//! Cloud secret stores. Each store shells out to its official CLI, reusing the user's existing login.
//! Secrets only ever reach a CLI through stdin, never argv (argv is visible to every local process).

pub mod aws;
pub mod azure;
pub mod gcp;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// `destination.store` in a spec: `{"type": "gcp_secret_manager", ...store options}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoreSpec {
    GcpSecretManager(gcp::Options),
    AwsSecretsManager(aws::SecretsManagerOptions),
    AwsParameterStore(aws::ParameterStoreOptions),
    AzureKeyVault(azure::Options),
}

impl StoreSpec {
    pub fn backend(&self) -> Box<dyn SecretStore> {
        match self {
            StoreSpec::GcpSecretManager(o) => gcp::backend(o),
            StoreSpec::AwsSecretsManager(o) => aws::secrets_manager(o),
            StoreSpec::AwsParameterStore(o) => aws::parameter_store(o),
            StoreSpec::AzureKeyVault(o) => azure::backend(o),
        }
    }

    /// The spec for a store the user ticked on the page. Starts from `suggested` when it's the same kind,
    /// so options the agent set (region, profile, KMS key) carry over.
    pub fn chosen(kind: &str, choice: Option<&str>, suggested: Option<&StoreSpec>) -> Option<StoreSpec> {
        let mut spec = match suggested.filter(|s| s.kind() == kind) {
            Some(s) => s.clone(),
            None => match kind {
                "gcp_secret_manager" => StoreSpec::GcpSecretManager(gcp::Options { project: None }),
                "aws_secrets_manager" => StoreSpec::AwsSecretsManager(Default::default()),
                "aws_parameter_store" => StoreSpec::AwsParameterStore(Default::default()),
                "azure_key_vault" => {
                    StoreSpec::AzureKeyVault(azure::Options { vault: String::new(), subscription: None })
                }
                _ => return None,
            },
        };
        match (&mut spec, choice) {
            (StoreSpec::GcpSecretManager(o), Some(c)) => o.project = Some(c.into()),
            (StoreSpec::AzureKeyVault(o), Some(c)) => o.vault = c.into(),
            (StoreSpec::AzureKeyVault(_), None) => return None,
            _ => {}
        }
        Some(spec)
    }

    /// The choice (project, vault) this spec points at, to preselect it on the page.
    pub fn choice(&self) -> Option<String> {
        match self {
            StoreSpec::GcpSecretManager(o) => o.project.clone(),
            StoreSpec::AzureKeyVault(o) => Some(o.vault.clone()),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            StoreSpec::GcpSecretManager(_) => "gcp_secret_manager",
            StoreSpec::AwsSecretsManager(_) => "aws_secrets_manager",
            StoreSpec::AwsParameterStore(_) => "aws_parameter_store",
            StoreSpec::AzureKeyVault(_) => "azure_key_vault",
        }
    }
}

pub trait SecretStore {
    /// For the page: (service, where), e.g. ("Google Cloud Secret Manager", "project acme-prod").
    fn describe(&self) -> (String, String);

    /// Runs before the user is asked anything, so a problem can't waste their input: CLI installed,
    /// signed in, target (project/vault/region) reachable, and every name valid for this store.
    /// Must not create or change anything. Errors should say how to fix the problem.
    fn preflight(&self, names: &[&str]) -> Result<()>;

    /// Stores `value` under `name`, creating the secret or adding a new version, and returns a
    /// reference to it (resource name, ARN or URL) that apps or tools can resolve later.
    fn put(&self, name: &str, value: &str) -> Result<String>;

    /// The name to store `wanted` under, for stores with stricter naming rules.
    fn name_for(&self, wanted: &str) -> String {
        wanted.to_string()
    }
}

/// A store found on this computer, offered on the page.
#[derive(Debug, Clone, Serialize)]
pub struct Offer {
    /// `StoreSpec` type tag.
    pub kind: &'static str,
    pub service: String,
    /// Account, region or similar.
    pub detail: String,
    /// Why it can't be used right now, and how to fix it; shown disabled. Having nothing to pick
    /// (no projects or vaults found) is worked out later, as the agent may suggest one.
    pub problem: Option<String>,
    /// What the user picks between (e.g. "Project"), when there's a choice to make.
    pub choice_label: Option<&'static str>,
    pub choices: Vec<String>,
    pub default_choice: Option<String>,
}

impl Offer {
    pub fn unavailable(kind: &'static str, service: impl Into<String>, problem: impl Into<String>) -> Offer {
        Offer {
            kind,
            service: service.into(),
            detail: String::new(),
            problem: Some(problem.into()),
            choice_label: None,
            choices: Vec::new(),
            default_choice: None,
        }
    }
}

/// Looks for usable stores in parallel (read-only CLI calls), giving up on any that take over 20s.
/// Stores whose CLI isn't installed are left out.
pub fn detect() -> Vec<Offer> {
    let detectors: [fn() -> Vec<Offer>; 3] = [gcp::detect, aws::detect, azure::detect];
    let (tx, rx) = mpsc::channel();
    for (i, detector) in detectors.into_iter().enumerate() {
        let tx = tx.clone();
        std::thread::spawn(move || tx.send((i, detector())));
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut found = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(r) => found.push(r),
            Err(_) => break,
        }
    }
    found.sort_by_key(|(i, _)| *i);
    found.into_iter().flat_map(|(_, offers)| offers).collect()
}

/// The first non-empty line of a CLI's error output, capped so it reads on the page.
pub fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    match line.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

/// True when `err` came from the CLI not being installed.
pub fn not_installed(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound))
}

/// Runs a CLI, feeding `input` on stdin, and returns stdout. Refuses to run if `secret` appears in
/// any argument, and removes it from error output.
pub fn run(mut cmd: Command, input: Option<&str>, secret: Option<&str>) -> Result<String> {
    let secret = secret.filter(|s| !s.is_empty());
    let shown = format!(
        "{} {}",
        cmd.get_program().to_string_lossy(),
        cmd.get_args().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
    );
    if let Some(s) = secret
        && cmd.get_args().any(|a| a.to_string_lossy().contains(s))
    {
        bail!("refusing to pass a secret as a command-line argument");
    }
    let redact = |text: &str| match secret {
        Some(s) => text.replace(s, "[redacted]"),
        None => text.to_string(),
    };
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("running `{shown}`"))?;
    if let Some(data) = input {
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(data.as_bytes()).with_context(|| format!("writing to `{shown}`"))?;
    }
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        bail!("`{shown}` failed: {}", first_line(&redact(detail)));
    }
    Ok(stdout)
}

/// Stand-in CLIs for tests, so no test ever touches a real cloud account.
#[cfg(all(test, unix))]
pub mod testing {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// Writes an executable `<dir>/<name>` that records each call's argv to `<dir>/args.<n>` (one per
    /// line) and stdin to `<dir>/stdin.<n>`, numbered from 1, then runs `body` (sh, with "$@").
    pub fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let d = dir.display();
        let script = format!(
            "#!/bin/sh\nn=$(( $(cat '{d}/count' 2>/dev/null || echo 0) + 1 ))\necho $n > '{d}/count'\n\
             printf '%s\\n' \"$@\" > '{d}/args.'$n\ncat > '{d}/stdin.'$n\n{body}\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// (argv, stdin) of call `n`.
    pub fn call(dir: &Path, n: usize) -> (Vec<String>, String) {
        let args = std::fs::read_to_string(dir.join(format!("args.{n}"))).unwrap_or_default();
        let stdin = std::fs::read_to_string(dir.join(format!("stdin.{n}"))).unwrap_or_default();
        (args.lines().map(String::from).collect(), stdin)
    }

    pub fn calls(dir: &Path) -> usize {
        std::fs::read_to_string(dir.join("count")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
    }

    pub fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentikeys-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn run_feeds_stdin_and_guards_argv() {
        let dir = testing::temp_dir("run");
        let cli = testing::fake_cli(&dir, "tool", "echo ok");
        assert_eq!(run(Command::new(&cli), Some("s3cret"), Some("s3cret")).unwrap().trim(), "ok");
        assert_eq!(testing::call(&dir, 1).1, "s3cret");

        let mut leaky = Command::new(&cli);
        leaky.arg("--value=s3cret");
        assert!(run(leaky, None, Some("s3cret")).is_err());
        assert_eq!(testing::calls(&dir), 1, "leaky command must not run");

        let failing = testing::fake_cli(&dir, "bad", "echo \"bad value s3cret\" >&2; exit 1");
        let err = run(Command::new(&failing), Some("s3cret"), Some("s3cret")).unwrap_err().to_string();
        assert!(err.contains("[redacted]") && !err.contains("s3cret"), "{err}");
    }

    #[test]
    fn errors_are_trimmed_to_one_readable_line() {
        assert_eq!(first_line("\n  ERROR: nope\nmore detail\n"), "ERROR: nope");
        let long = first_line(&"x".repeat(500));
        assert_eq!(long.chars().count(), 301);
        assert!(long.ends_with('…'));
    }
}
