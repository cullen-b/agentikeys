//! AWS Secrets Manager and Systems Manager Parameter Store via the `aws` CLI.

use super::{Offer, SecretStore, not_installed, run};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::process::Command;

const INSTALL_URL: &str = "https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html";

/// Shared by both services; they differ only in what `kms_key_id` means (Secrets Manager `KmsKeyId`,
/// Parameter Store `KeyId`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsManagerOptions {
    pub region: Option<String>,
    pub profile: Option<String>,
    pub kms_key_id: Option<String>,
}

pub type ParameterStoreOptions = SecretsManagerOptions;

/// Offers Secrets Manager and Parameter Store when aws is signed in with a region.
pub fn detect() -> Vec<Offer> {
    detect_with(&Aws::new(Service::SecretsManager, &SecretsManagerOptions::default(), "aws"))
}

fn detect_with(aws: &Aws) -> Vec<Offer> {
    let (detail, problem) = match run(aws.command(&["sts", "get-caller-identity"]), None, None) {
        Err(e) if not_installed(&e) => return Vec::new(),
        Err(_) => (String::new(), Some("Not signed in: run `aws configure` or `aws sso login`".to_string())),
        Ok(out) => {
            let account = serde_json::from_str::<Value>(&out).ok().and_then(|v| v["Account"].as_str().map(String::from));
            match aws.region() {
                Some(r) => (format!("Account {} · {r}", account.unwrap_or_default()), None),
                None => (String::new(), Some("No region set: run `aws configure set region <region>`".to_string())),
            }
        }
    };
    [("aws_secrets_manager", "AWS Secrets Manager"), ("aws_parameter_store", "AWS Systems Manager Parameter Store")]
        .into_iter()
        .map(|(kind, service)| Offer {
            kind,
            service: service.into(),
            detail: detail.clone(),
            problem: problem.clone(),
            choice_label: None,
            choices: Vec::new(),
            default_choice: None,
        })
        .collect()
}

pub fn secrets_manager(opts: &SecretsManagerOptions) -> Box<dyn SecretStore> {
    Box::new(Aws::new(Service::SecretsManager, opts, "aws"))
}

pub fn parameter_store(opts: &ParameterStoreOptions) -> Box<dyn SecretStore> {
    Box::new(Aws::new(Service::ParameterStore, opts, "aws"))
}

#[derive(Clone, Copy, PartialEq)]
enum Service {
    SecretsManager,
    ParameterStore,
}

struct Aws {
    service: Service,
    bin: String,
    opts: SecretsManagerOptions,
}

impl Aws {
    fn new(service: Service, opts: &SecretsManagerOptions, bin: &str) -> Self {
        Self { service, bin: bin.into(), opts: opts.clone() }
    }

    fn label(&self) -> &'static str {
        match self.service {
            Service::SecretsManager => "AWS Secrets Manager",
            Service::ParameterStore => "AWS Systems Manager Parameter Store",
        }
    }

    /// `aws [--region R] [--profile P] <service> <op> ... --output json`.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        if let Some(r) = &self.opts.region {
            cmd.args(["--region", r]);
        }
        if let Some(p) = &self.opts.profile {
            cmd.args(["--profile", p]);
        }
        cmd.args(args).args(["--output", "json"]);
        cmd
    }

    /// Sends the request as JSON on stdin: the CLI has no `-` for stdin, and this also keeps AWS CLI
    /// from fetching a value that looks like a URL. Unix-only, as `/dev/stdin` doesn't exist on Windows.
    fn call_json(&self, service: &str, op: &str, request: &Value, secret: Option<&str>) -> Result<Value> {
        let cmd = self.command(&[service, op, "--cli-input-json", "file:///dev/stdin"]);
        let input = zeroize::Zeroizing::new(request.to_string());
        let out = run(cmd, Some(&input), secret)?;
        serde_json::from_str(&out).with_context(|| format!("reading the response of `aws {service} {op}`"))
    }

    fn secrets_manager_put(&self, name: &str, value: &str) -> Result<String> {
        let exists = match self.call_json("secretsmanager", "describe-secret", &json!({ "SecretId": name }), None) {
            Ok(_) => true,
            Err(e) if format!("{e:#}").contains("ResourceNotFoundException") => false,
            Err(e) => return Err(e),
        };
        let reply = if exists {
            let request = json!({ "SecretId": name, "SecretString": value });
            self.call_json("secretsmanager", "put-secret-value", &request, Some(value))?
        } else {
            let mut request = json!({ "Name": name, "SecretString": value });
            if let Some(key) = &self.opts.kms_key_id {
                request["KmsKeyId"] = json!(key);
            }
            self.call_json("secretsmanager", "create-secret", &request, Some(value))?
        };
        reply["ARN"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow!("AWS Secrets Manager saved '{name}' but returned no ARN"))
    }

    /// STS works without a region but the secret services don't, so catch a missing one before the page opens.
    fn check_region(&self) -> Result<()> {
        if self.region().is_none() {
            bail!("no AWS region is set: add \"region\" to destination.store, or run `aws configure set region <region>`");
        }
        Ok(())
    }

    /// The region from the spec, the environment, or `aws configure`.
    fn region(&self) -> Option<String> {
        let from_env = ["AWS_REGION", "AWS_DEFAULT_REGION"].iter().find_map(|v| std::env::var(v).ok().filter(|r| !r.is_empty()));
        if let Some(r) = self.opts.region.clone().or(from_env) {
            return Some(r);
        }
        let mut cmd = Command::new(&self.bin);
        cmd.args(["configure", "get", "region"]);
        if let Some(p) = &self.opts.profile {
            cmd.args(["--profile", p]);
        }
        // `configure get` exits non-zero when the value is unset.
        run(cmd, None, None).ok().map(|r| r.trim().to_string()).filter(|r| !r.is_empty())
    }

    fn parameter_store_put(&self, name: &str, value: &str) -> Result<String> {
        let mut request = json!({ "Name": name, "Value": value, "Type": "SecureString", "Overwrite": true });
        if let Some(key) = &self.opts.kms_key_id {
            request["KeyId"] = json!(key);
        }
        self.call_json("ssm", "put-parameter", &request, Some(value))?;
        Ok(name.to_string())
    }
}

impl SecretStore for Aws {
    fn describe(&self) -> (String, String) {
        let mut place = self.opts.region.clone().unwrap_or_else(|| "default region".into());
        if let Some(p) = &self.opts.profile {
            place.push_str(&format!(" · profile {p}"));
        }
        (self.label().into(), place)
    }

    fn preflight(&self, names: &[&str]) -> Result<()> {
        if !cfg!(unix) {
            bail!("{} isn't supported on Windows yet (the aws CLI is fed through /dev/stdin); use macOS or Linux", self.label());
        }
        for name in names {
            let rule = match self.service {
                Service::SecretsManager => secret_name_problem(name),
                Service::ParameterStore => parameter_name_problem(name),
            };
            if let Some(rule) = rule {
                bail!("'{name}' isn't a valid {} name: {rule}. Set `\"secret\"` on that field to a valid name.", self.label());
            }
        }
        let identity = run(self.command(&["sts", "get-caller-identity"]), None, None).map_err(|e| {
            if not_installed(&e) {
                anyhow!("the aws CLI isn't installed. Install it from {INSTALL_URL}, then run `aws configure` or `aws sso login`")
            } else {
                let profile = self.opts.profile.as_ref().map(|p| format!(" --profile {p}")).unwrap_or_default();
                anyhow!("not signed in to AWS: {e:#}\nRun `aws configure` or `aws sso login{profile}`, then try again")
            }
        })?;
        serde_json::from_str::<Value>(&identity).ok().filter(|v| v["Account"].is_string()).ok_or_else(|| {
            anyhow!("`aws sts get-caller-identity` returned something unexpected; check that `aws --version` reports AWS CLI v2")
        })?;
        self.check_region()
    }

    fn put(&self, name: &str, value: &str) -> Result<String> {
        match self.service {
            Service::SecretsManager => self.secrets_manager_put(name, value),
            Service::ParameterStore => self.parameter_store_put(name, value),
        }
    }
}

/// Secrets Manager: 1-512 ASCII letters, digits or `/_+=.@-`.
fn secret_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.len() > 512 {
        return Some("it must be 1-512 characters");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || "/_+=.@-".contains(c)) {
        return Some("use only ASCII letters, digits and /_+=.@-");
    }
    None
}

/// Parameter Store: `[A-Za-z0-9_.-]` segments, optionally a `/`-separated path (leading `/`, at most
/// 15 levels), not starting with `aws` or `ssm`. The documented 1011-character limit includes the
/// ARN prefix (45 characters for us-east-2), hence 966.
fn parameter_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.len() > 966 {
        return Some("it must be 1-966 characters");
    }
    let path = name.strip_prefix('/');
    let segments: Vec<&str> = path.unwrap_or(name).split('/').collect();
    if segments.iter().any(|s| s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))) {
        return Some("use only letters, digits and _.- in each /-separated part (no spaces or empty parts)");
    }
    if path.is_none() && segments.len() > 1 {
        return Some("a name with / must start with / (for example /app/prod/api-key)");
    }
    if segments.len() > 15 {
        return Some("hierarchies are limited to 15 levels");
    }
    let lower = path.unwrap_or(name).to_ascii_lowercase();
    if lower.starts_with("aws") || lower.starts_with("ssm") {
        return Some("names can't start with 'aws' or 'ssm' (reserved by AWS)");
    }
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::stores::testing::{call, calls, fake_cli, temp_dir};
    use std::path::Path;

    const SECRET: &str = "s3cr3t-v@lue";
    const ARN: &str = "arn:aws:secretsmanager:us-east-1:123456789012:secret:API_KEY-a1b2c3";

    fn backend(service: Service, dir: &Path, body: &str, opts: SecretsManagerOptions) -> Aws {
        let cli = fake_cli(dir, "aws", body);
        Aws::new(service, &opts, cli.to_str().unwrap())
    }

    fn opts(region: Option<&str>, profile: Option<&str>, key: Option<&str>) -> SecretsManagerOptions {
        SecretsManagerOptions {
            region: region.map(String::from),
            profile: profile.map(String::from),
            kms_key_id: key.map(String::from),
        }
    }

    fn json_of(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn assert_secret_only_on_stdin(dir: &Path) {
        for n in 1..=calls(dir) {
            let (argv, _) = call(dir, n);
            assert!(!argv.iter().any(|a| a.contains(SECRET)), "secret in argv of call {n}: {argv:?}");
        }
    }

    // Describes as missing, then create-secret returns the ARN.
    const MISSING_THEN_CREATE: &str = r#"
if [ "$n" = 1 ]; then echo "An error occurred (ResourceNotFoundException) when calling the DescribeSecret operation: Secrets Manager can't find the specified secret." >&2; exit 254; fi
echo '{"ARN":"arn:aws:secretsmanager:us-east-1:123456789012:secret:API_KEY-a1b2c3","Name":"API_KEY"}'"#;

    #[test]
    fn secrets_manager_creates_missing_secret() {
        let dir = temp_dir("aws-sm-create");
        let store = backend(Service::SecretsManager, &dir, MISSING_THEN_CREATE, opts(Some("eu-west-1"), Some("work"), Some("alias/mine")));
        assert_eq!(store.put("API_KEY", SECRET).unwrap(), ARN);
        assert_eq!(calls(&dir), 2);

        let (argv, stdin) = call(&dir, 1);
        let expected = ["--region", "eu-west-1", "--profile", "work", "secretsmanager", "describe-secret"];
        assert_eq!(argv[..6], expected);
        assert_eq!(argv[6..], ["--cli-input-json", "file:///dev/stdin", "--output", "json"]);
        assert_eq!(json_of(&stdin), json!({ "SecretId": "API_KEY" }));

        let (argv, stdin) = call(&dir, 2);
        assert_eq!(
            argv,
            ["--region", "eu-west-1", "--profile", "work", "secretsmanager", "create-secret",
             "--cli-input-json", "file:///dev/stdin", "--output", "json"]
        );
        assert_eq!(json_of(&stdin), json!({ "Name": "API_KEY", "SecretString": SECRET, "KmsKeyId": "alias/mine" }));
        assert_secret_only_on_stdin(&dir);
    }

    #[test]
    fn secrets_manager_updates_existing_secret() {
        let dir = temp_dir("aws-sm-update");
        let body = r#"echo '{"ARN":"arn:aws:secretsmanager:us-east-1:123456789012:secret:API_KEY-a1b2c3","Name":"API_KEY"}'"#;
        let store = backend(Service::SecretsManager, &dir, body, opts(None, None, Some("alias/mine")));
        assert_eq!(store.put("API_KEY", SECRET).unwrap(), ARN);
        assert_eq!(calls(&dir), 2);
        assert_eq!(call(&dir, 1).0[0], "secretsmanager");

        let (argv, stdin) = call(&dir, 2);
        assert_eq!(argv, ["secretsmanager", "put-secret-value", "--cli-input-json", "file:///dev/stdin", "--output", "json"]);
        assert_eq!(json_of(&stdin), json!({ "SecretId": "API_KEY", "SecretString": SECRET }));
        assert_secret_only_on_stdin(&dir);
    }

    #[test]
    fn secrets_manager_access_denied_is_not_treated_as_missing() {
        let dir = temp_dir("aws-sm-denied");
        let body = "echo 'An error occurred (AccessDeniedException) when calling the DescribeSecret operation: not allowed' >&2; exit 254";
        let store = backend(Service::SecretsManager, &dir, body, SecretsManagerOptions::default());
        let err = format!("{:#}", store.put("API_KEY", SECRET).unwrap_err());
        assert!(err.contains("AccessDeniedException"), "{err}");
        assert_eq!(calls(&dir), 1, "must not try to create after an access error");
    }

    #[test]
    fn secrets_manager_failure_does_not_leak_secret() {
        let dir = temp_dir("aws-sm-leak");
        let body = format!(
            "if [ \"$n\" = 1 ]; then echo '{{}}'; exit 0; fi\necho \"An error occurred (InvalidRequestException): bad value {SECRET}\" >&2; exit 254"
        );
        let store = backend(Service::SecretsManager, &dir, &body, SecretsManagerOptions::default());
        let err = format!("{:#}", store.put("API_KEY", SECRET).unwrap_err());
        assert!(err.contains("InvalidRequestException") && err.contains("[redacted]") && !err.contains(SECRET), "{err}");
    }

    #[test]
    fn parameter_store_overwrites_secure_string() {
        let dir = temp_dir("aws-ssm-put");
        let store = backend(Service::ParameterStore, &dir, r#"echo '{"Version":3,"Tier":"Standard"}'"#, opts(Some("us-west-2"), None, Some("alias/mine")));
        assert_eq!(store.put("/app/prod/api-key", SECRET).unwrap(), "/app/prod/api-key");
        assert_eq!(calls(&dir), 1);

        let (argv, stdin) = call(&dir, 1);
        assert_eq!(
            argv,
            ["--region", "us-west-2", "ssm", "put-parameter", "--cli-input-json", "file:///dev/stdin", "--output", "json"]
        );
        assert_eq!(
            json_of(&stdin),
            json!({ "Name": "/app/prod/api-key", "Value": SECRET, "Type": "SecureString", "Overwrite": true, "KeyId": "alias/mine" })
        );
        assert_secret_only_on_stdin(&dir);
    }

    #[test]
    fn parameter_store_omits_key_id_by_default_and_hides_secret_on_failure() {
        let dir = temp_dir("aws-ssm-default");
        let store = backend(Service::ParameterStore, &dir, "echo '{}'", SecretsManagerOptions::default());
        store.put("API_KEY", SECRET).unwrap();
        assert_eq!(json_of(&call(&dir, 1).1), json!({ "Name": "API_KEY", "Value": SECRET, "Type": "SecureString", "Overwrite": true }));

        let dir = temp_dir("aws-ssm-leak");
        let body = format!("echo \"An error occurred (ValidationException): {SECRET}\" >&2; exit 254");
        let store = backend(Service::ParameterStore, &dir, &body, SecretsManagerOptions::default());
        let err = format!("{:#}", store.put("API_KEY", SECRET).unwrap_err());
        assert!(err.contains("[redacted]") && !err.contains(SECRET), "{err}");
    }

    #[test]
    fn describe_shows_region_and_profile() {
        let o = opts(Some("eu-west-1"), Some("work"), None);
        assert_eq!(Aws::new(Service::SecretsManager, &o, "aws").describe(), ("AWS Secrets Manager".into(), "eu-west-1 · profile work".into()));
        assert_eq!(
            Aws::new(Service::ParameterStore, &SecretsManagerOptions::default(), "aws").describe(),
            ("AWS Systems Manager Parameter Store".into(), "default region".into())
        );
    }

    #[test]
    fn preflight_requires_a_region() {
        if ["AWS_REGION", "AWS_DEFAULT_REGION"].iter().any(|v| std::env::var(v).is_ok_and(|r| !r.is_empty())) {
            return;
        }
        let dir = temp_dir("aws-no-region");
        let body = r#"case "$*" in *sts*) echo '{"Account":"123456789012"}';; *configure*) exit 1;; esac"#;
        let store = backend(Service::SecretsManager, &dir, body, opts(None, Some("work"), None));
        let err = store.preflight(&["API_KEY"]).unwrap_err().to_string();
        assert!(err.contains("no AWS region"), "{err}");
        assert_eq!(call(&dir, 2).0, ["configure", "get", "region", "--profile", "work"]);
    }

    #[test]
    fn preflight_checks_identity_with_region_and_profile() {
        let dir = temp_dir("aws-pre-ok");
        let store = backend(Service::SecretsManager, &dir, r#"echo '{"Account":"123456789012","Arn":"arn:aws:iam::123456789012:user/me"}'"#, opts(Some("eu-west-1"), Some("work"), None));
        store.preflight(&["API_KEY", "db/pass.1"]).unwrap();
        assert_eq!(calls(&dir), 1);
        assert_eq!(
            call(&dir, 1).0,
            ["--region", "eu-west-1", "--profile", "work", "sts", "get-caller-identity", "--output", "json"]
        );
    }

    #[test]
    fn preflight_not_signed_in() {
        let dir = temp_dir("aws-pre-auth");
        let body = "echo 'Unable to locate credentials. You can configure credentials by running \"aws configure\".' >&2; exit 253";
        let store = backend(Service::ParameterStore, &dir, body, opts(None, Some("work"), None));
        let err = store.preflight(&["API_KEY"]).unwrap_err().to_string();
        assert!(err.contains("not signed in") && err.contains("aws sso login --profile work") && err.contains("aws configure"), "{err}");
    }

    #[test]
    fn preflight_cli_not_installed() {
        let store = Aws::new(Service::SecretsManager, &SecretsManagerOptions::default(), "/nonexistent/agentikeys-aws");
        let err = store.preflight(&["API_KEY"]).unwrap_err().to_string();
        assert!(err.contains("isn't installed") && err.contains(INSTALL_URL), "{err}");
    }

    #[test]
    fn preflight_rejects_invalid_names_without_calling_aws() {
        let dir = temp_dir("aws-pre-names");
        let sm = backend(Service::SecretsManager, &dir, "echo '{}'", SecretsManagerOptions::default());
        let err = sm.preflight(&["GOOD", "has space"]).unwrap_err().to_string();
        assert!(err.contains("'has space'") && err.contains("AWS Secrets Manager") && err.contains("ASCII letters"), "{err}");
        assert!(sm.preflight(&[&"x".repeat(513)]).is_err());

        let ssm = backend(Service::ParameterStore, &dir, "echo '{}'", SecretsManagerOptions::default());
        for bad in ["a/b", "/a//b", "/a/", "AWS_KEY", "/ssm/x", "has space", "a@b", ""] {
            assert!(ssm.preflight(&[bad]).is_err(), "{bad:?} should be rejected");
        }
        let deep = "/a".repeat(16);
        assert!(ssm.preflight(&[&deep]).unwrap_err().to_string().contains("15 levels"));
        assert_eq!(calls(&dir), 0, "name checks must not reach the CLI");
    }

    #[test]
    fn valid_names_are_accepted() {
        for ok in ["API_KEY", "db/pass+1=x.y@z-w", "a"] {
            assert_eq!(secret_name_problem(ok), None, "{ok}");
        }
        for ok in ["API_KEY", "/app/prod/api-key", "my.param_1-x", "/a/b/c"] {
            assert_eq!(parameter_name_problem(ok), None, "{ok}");
        }
    }

    #[test]
    fn options_reject_unknown_fields() {
        assert!(serde_json::from_value::<SecretsManagerOptions>(json!({ "region": "us-east-1", "nope": 1 })).is_err());
        let o: SecretsManagerOptions = serde_json::from_value(json!({ "profile": "p" })).unwrap();
        assert_eq!(o.profile.as_deref(), Some("p"));
    }
}
