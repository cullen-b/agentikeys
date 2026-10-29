//! Google Cloud Secret Manager via `gcloud`, reusing the user's existing `gcloud auth login`.

use super::{Offer, SecretStore, run};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// Project ID; defaults to gcloud's configured project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

pub fn backend(opts: &Options) -> Box<dyn SecretStore> {
    Box::new(Gcp::new("gcloud", opts.project.clone()))
}

struct Gcp {
    bin: String,
    project: OnceLock<String>,
}

impl Gcp {
    fn new(bin: &str, project: Option<String>) -> Self {
        let cell = OnceLock::new();
        if let Some(p) = project.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()) {
            let _ = cell.set(p);
        }
        Gcp { bin: bin.into(), project: cell }
    }

    fn gcloud(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args).env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1");
        cmd
    }

    fn project(&self) -> Result<&str> {
        if let Some(p) = self.project.get() {
            return Ok(p);
        }
        let out = run(self.gcloud(&["config", "get-value", "project"]), None, None)?;
        let p = out.trim();
        if p.is_empty() || p == "(unset)" {
            bail!(
                "no Google Cloud project: set \"project\" in destination.store, or run \
                 `gcloud config set project <id>`"
            );
        }
        Ok(self.project.get_or_init(|| p.to_string()))
    }

    fn secret_exists(&self, project: &str, name: &str) -> Result<bool> {
        let args = ["secrets", "describe", name, "--project", project, "--format=value(name)"];
        let out =
            self.gcloud(&args).output().map_err(|e| anyhow::anyhow!("running `{} secrets describe`: {e}", self.bin))?;
        if out.status.success() {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("NOT_FOUND") {
            return Ok(false);
        }
        bail!(
            "couldn't check whether secret `{name}` exists in project {project}: {}. The signed-in \
             account needs roles/secretmanager.viewer or better on the project",
            stderr.trim()
        )
    }
}

use super::not_installed;

const KIND: &str = "gcp_secret_manager";
const SERVICE: &str = "Google Cloud Secret Manager";

/// Offers Secret Manager when gcloud is signed in, with the account's projects to pick from.
pub fn detect() -> Vec<Offer> {
    detect_with(&Gcp::new("gcloud", None))
}

fn detect_with(g: &Gcp) -> Vec<Offer> {
    let list = ["auth", "list", "--filter=status:ACTIVE", "--format=value(account)"];
    let account = match run(g.gcloud(&list), None, None) {
        Err(e) if not_installed(&e) => return Vec::new(),
        Err(e) => return vec![Offer::unavailable(KIND, SERVICE, format!("gcloud failed: {e:#}"))],
        Ok(out) => out.lines().next().unwrap_or("").trim().to_string(),
    };
    if account.is_empty() {
        return vec![Offer::unavailable(KIND, SERVICE, "Not signed in: run `gcloud auth login`")];
    }
    let projects = ["projects", "list", "--format=value(projectId)", "--sort-by=projectId", "--limit=500"];
    let mut choices: Vec<String> = run(g.gcloud(&projects), None, None)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect();
    let default = g.project().ok().map(String::from);
    if let Some(d) = &default
        && !choices.contains(d)
    {
        choices.insert(0, d.clone());
    }
    vec![Offer {
        kind: KIND,
        service: SERVICE.into(),
        detail: format!("Signed in as {account}"),
        problem: None,
        choice_label: Some("Project"),
        choices,
        default_choice: default,
    }]
}

fn validate_name(name: &str) -> Result<()> {
    let ok = (1..=255).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        bail!(
            "`{name}` isn't a valid Secret Manager secret ID: use 1-255 letters, digits, `-` or `_` \
             (set the field's \"secret\" to a valid ID)"
        );
    }
    Ok(())
}

impl SecretStore for Gcp {
    fn describe(&self) -> (String, String) {
        let project = self.project.get().map(String::as_str).unwrap_or("(gcloud default)");
        ("Google Cloud Secret Manager".into(), format!("project {project}"))
    }

    fn preflight(&self, names: &[&str]) -> Result<()> {
        let list = ["auth", "list", "--filter=status:ACTIVE", "--format=value(account)"];
        let accounts = run(self.gcloud(&list), None, None).map_err(|e| {
            if not_installed(&e) {
                anyhow::anyhow!(
                    "`{}` isn't installed: install the Google Cloud CLI from \
                     https://cloud.google.com/sdk/docs/install",
                    self.bin
                )
            } else {
                e
            }
        })?;
        if accounts.trim().is_empty() {
            bail!("gcloud isn't signed in: run `gcloud auth login`");
        }
        let project = self.project()?;
        names.iter().try_for_each(|n| validate_name(n))?;
        // Read-only probe: catches a disabled API or missing permission before anything is saved.
        let probe = ["secrets", "list", "--project", project, "--limit=1", "--format=value(name)"];
        let out =
            self.gcloud(&probe).output().map_err(|e| anyhow::anyhow!("running `{} secrets list`: {e}", self.bin))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("SERVICE_DISABLED") {
            bail!(
                "Secret Manager isn't turned on in project {project}. Turn it on with `gcloud services enable \
                 secretmanager.googleapis.com --project {project}`, wait a minute, then save again."
            );
        }
        if err.contains("PERMISSION_DENIED") || err.contains("does not have permission") {
            bail!(
                "your Google account can't use Secret Manager in project {project}. It needs the Secret Manager \
                 Admin role (roles/secretmanager.admin) there."
            );
        }
        bail!("can't use Secret Manager in project {project}: {}", super::first_line(&err))
    }

    fn put(&self, name: &str, value: &str) -> Result<String> {
        validate_name(name)?;
        let project = self.project()?;
        if !self.secret_exists(project, name)? {
            let create = ["secrets", "create", name, "--project", project, "--replication-policy=automatic"];
            run(self.gcloud(&create), None, None)?;
        }
        let add = ["secrets", "versions", "add", name, "--project", project, "--data-file=-", "--format=value(name)"];
        let out = run(self.gcloud(&add), Some(value), Some(value))?;
        let reference = out.trim();
        if reference.is_empty() {
            bail!("gcloud added a version of `{name}` but didn't report its resource name");
        }
        Ok(reference.to_string())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::testing::{call, calls, fake_cli, temp_dir};
    use super::*;

    const SECRET: &str = "s3cr3t-value";

    fn gcp(dir: &std::path::Path, body: &str, project: Option<&str>) -> Gcp {
        Gcp::new(fake_cli(dir, "gcloud", body).to_str().unwrap(), project.map(String::from))
    }

    // Routes by subcommand; `describe` exits `rc` printing `err` on stderr.
    fn router(rc: i32, err: &str) -> String {
        format!(
            r#"case "$1 $2" in
"auth list") echo me@example.com;;
"config get-value") echo my-proj;;
"secrets describe") echo "{err}" >&2; exit {rc};;
"secrets create") ;;
"secrets list") ;;
"secrets versions") echo projects/123/secrets/$4/versions/7;;
esac"#
        )
    }

    #[test]
    fn describe_names_project() {
        let dir = temp_dir("gcp-describe");
        let g = gcp(&dir, "true", Some("acme-prod"));
        assert_eq!(g.describe(), ("Google Cloud Secret Manager".into(), "project acme-prod".into()));
    }

    #[test]
    fn put_creates_missing_secret_then_adds_version() {
        let dir = temp_dir("gcp-create");
        let g = gcp(&dir, &router(1, "NOT_FOUND: Secret [x] not found"), Some("acme"));
        let reference = g.put("API_KEY", SECRET).unwrap();
        assert_eq!(reference, "projects/123/secrets/API_KEY/versions/7");
        assert_eq!(calls(&dir), 3);
        let args = |n| call(&dir, n).0.join(" ");
        assert_eq!(args(1), "secrets describe API_KEY --project acme --format=value(name)");
        assert_eq!(args(2), "secrets create API_KEY --project acme --replication-policy=automatic");
        assert_eq!(args(3), "secrets versions add API_KEY --project acme --data-file=- --format=value(name)");
        for n in 1..=3 {
            let (argv, stdin) = call(&dir, n);
            assert!(!argv.iter().any(|a| a.contains(SECRET)), "secret in argv of call {n}");
            assert_eq!(stdin, if n == 3 { SECRET } else { "" }, "stdin of call {n}");
        }
    }

    #[test]
    fn put_existing_secret_only_adds_version() {
        let dir = temp_dir("gcp-existing");
        let g = gcp(&dir, &router(0, ""), Some("acme"));
        g.put("API_KEY", SECRET).unwrap();
        assert_eq!(calls(&dir), 2);
        assert_eq!(call(&dir, 1).0[..2], ["secrets", "describe"]);
        assert_eq!(call(&dir, 2).0[..3], ["secrets", "versions", "add"]);
        assert_eq!(call(&dir, 2).1, SECRET);
    }

    #[test]
    fn put_does_not_treat_permission_errors_as_missing() {
        let dir = temp_dir("gcp-denied");
        let g = gcp(&dir, &router(1, "PERMISSION_DENIED: nope"), Some("acme"));
        let err = g.put("API_KEY", SECRET).unwrap_err().to_string();
        assert!(err.contains("PERMISSION_DENIED") && err.contains("secretmanager.viewer"), "{err}");
        assert_eq!(calls(&dir), 1, "must not create or add after a failed existence check");
    }

    #[test]
    fn put_uses_gcloud_default_project() {
        let dir = temp_dir("gcp-default-project");
        let g = gcp(&dir, &router(0, ""), None);
        g.put("K", SECRET).unwrap();
        assert_eq!(call(&dir, 1).0.join(" "), "config get-value project");
        assert!(call(&dir, 2).0.join(" ").contains("--project my-proj"));
        assert_eq!(g.describe().1, "project my-proj");
    }

    #[test]
    fn failing_gcloud_error_does_not_leak_secret() {
        let dir = temp_dir("gcp-leak");
        let body = format!(
            "case \"$1 $2\" in \"secrets describe\") exit 0;; *) echo \"bad value {SECRET}\" >&2; exit 1;; esac"
        );
        let err = gcp(&dir, &body, Some("acme")).put("K", SECRET).unwrap_err().to_string();
        assert!(err.contains("[redacted]") && !err.contains(SECRET), "{err}");
    }

    #[test]
    fn preflight_passes_without_creating_anything() {
        let dir = temp_dir("gcp-preflight-ok");
        let g = gcp(&dir, &router(0, ""), None);
        g.preflight(&["A", "b_c-1"]).unwrap();
        assert_eq!(calls(&dir), 3);
        assert_eq!(call(&dir, 1).0.join(" "), "auth list --filter=status:ACTIVE --format=value(account)");
        assert_eq!(call(&dir, 2).0.join(" "), "config get-value project");
        assert_eq!(call(&dir, 3).0.join(" "), "secrets list --project my-proj --limit=1 --format=value(name)");
    }

    #[test]
    fn preflight_reports_disabled_api() {
        let dir = temp_dir("gcp-disabled");
        let body = r#"case "$1 $2" in "auth list") echo me@example.com;; "secrets list") echo "SERVICE_DISABLED" >&2; exit 1;; esac"#;
        let err = gcp(&dir, body, Some("acme")).preflight(&["A"]).unwrap_err().to_string();
        assert!(err.contains("isn't turned on") && err.contains("gcloud services enable"), "{err}");

        let denied = r#"case "$1 $2" in "auth list") echo me@example.com;; "secrets list") echo "ERROR: PERMISSION_DENIED" >&2; exit 1;; esac"#;
        let err = gcp(&temp_dir("gcp-denied-probe"), denied, Some("acme")).preflight(&["A"]).unwrap_err().to_string();
        assert!(err.contains("roles/secretmanager.admin"), "{err}");
    }

    #[test]
    fn preflight_not_installed() {
        let dir = temp_dir("gcp-missing");
        let g = Gcp::new(dir.join("nope").to_str().unwrap(), Some("acme".into()));
        let err = g.preflight(&["A"]).unwrap_err().to_string();
        assert!(err.contains("isn't installed") && err.contains("cloud.google.com/sdk/docs/install"), "{err}");
    }

    #[test]
    fn preflight_not_signed_in() {
        let dir = temp_dir("gcp-signedout");
        let err = gcp(&dir, "true", Some("acme")).preflight(&["A"]).unwrap_err().to_string();
        assert!(err.contains("gcloud auth login"), "{err}");
    }

    #[test]
    fn preflight_no_project() {
        for out in ["", "(unset)"] {
            let dir = temp_dir("gcp-noproject");
            let body = format!("case \"$1\" in auth) echo me@example.com;; *) echo '{out}';; esac");
            let err = gcp(&dir, &body, None).preflight(&["A"]).unwrap_err().to_string();
            assert!(err.contains("gcloud config set project"), "{err}");
        }
    }

    #[test]
    fn preflight_rejects_invalid_names() {
        let dir = temp_dir("gcp-badname");
        let g = gcp(&dir, &router(0, ""), Some("acme"));
        for bad in ["has space", "dot.ted", "", &"x".repeat(256)] {
            let err = g.preflight(&["OK", bad]).unwrap_err().to_string();
            assert!(err.contains("Secret Manager secret ID") && err.contains("1-255"), "{err}");
        }
        assert!(g.preflight(&[&"x".repeat(255)]).is_ok());
    }

    #[test]
    fn detect_offers_projects_with_default_first() {
        let dir = temp_dir("gcp-detect");
        let body = r#"case "$1 $2" in
"auth list") echo me@example.com;;
"projects list") printf 'alpha\nbeta\n';;
"config get-value") echo zeta;;
esac"#;
        let offers = detect_with(&gcp(&dir, body, None));
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].choices, ["zeta", "alpha", "beta"]);
        assert_eq!(offers[0].default_choice.as_deref(), Some("zeta"));
        assert!(offers[0].problem.is_none() && offers[0].detail.contains("me@example.com"));
        assert!(detect_with(&Gcp::new(dir.join("nope").to_str().unwrap(), None)).is_empty());
    }

    #[test]
    fn options_reject_unknown_fields() {
        assert!(serde_json::from_str::<Options>(r#"{"project":"p","region":"x"}"#).is_err());
        assert!(serde_json::from_str::<Options>("{}").unwrap().project.is_none());
    }
}
