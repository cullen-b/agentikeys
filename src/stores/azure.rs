//! Azure Key Vault secrets via the `az` CLI. Every `put` adds a new version, so there's no create/update split.

use super::{Offer, SecretStore, not_installed};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::process::Command;

const INSTALL_URL: &str = "https://learn.microsoft.com/cli/azure/install-azure-cli";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// Key Vault name (not its URL).
    pub vault: String,
    /// Subscription name or ID; defaults to the one `az` has selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<String>,
}

const KIND: &str = "azure_key_vault";
const SERVICE: &str = "Azure Key Vault";

/// Offers Key Vault when az is signed in, with the subscription's vaults to pick from.
pub fn detect() -> Vec<Offer> {
    detect_with(&KeyVault { bin: "az".into(), opts: Options { vault: String::new(), subscription: None } })
}

fn detect_with(kv: &KeyVault) -> Vec<Offer> {
    let user = match super::run(
        kv.az(&["account", "show", "--query", "user.name", "-o", "tsv", "--only-show-errors"]),
        None,
        None,
    ) {
        Err(e) if not_installed(&e) => return Vec::new(),
        Err(_) => return vec![Offer::unavailable(KIND, SERVICE, "Not signed in: run `az login`")],
        Ok(out) => out.trim().to_string(),
    };
    let list = ["keyvault", "list", "--query", "[].name", "-o", "tsv", "--only-show-errors"];
    let vaults: Vec<String> = super::run(kv.az(&list), None, None)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
        .collect();
    vec![Offer {
        kind: KIND,
        service: SERVICE.into(),
        detail: format!("Signed in as {user}"),
        problem: None,
        choice_label: Some("Vault"),
        default_choice: vaults.first().cloned(),
        choices: vaults,
    }]
}

pub fn backend(opts: &Options) -> Box<dyn SecretStore> {
    Box::new(KeyVault { bin: "az".into(), opts: opts.clone() })
}

struct KeyVault {
    bin: String,
    opts: Options,
}

impl KeyVault {
    fn az(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        if let Some(s) = &self.opts.subscription {
            cmd.args(["--subscription", s]);
        }
        cmd
    }
}

impl SecretStore for KeyVault {
    fn describe(&self) -> (String, String) {
        ("Azure Key Vault".into(), format!("vault {}", self.opts.vault))
    }

    // No vault reachability check: any read (list/get) can need permissions a write-only identity lacks.
    fn preflight(&self, names: &[&str]) -> Result<()> {
        check_vault_name(&self.opts.vault)?;
        check_secret_names(names)?;
        if let Err(e) = super::run(self.az(&["account", "show", "--only-show-errors", "-o", "none"]), None, None) {
            let missing =
                e.chain().filter_map(|c| c.downcast_ref::<io::Error>()).any(|io| io.kind() == io::ErrorKind::NotFound);
            if missing {
                bail!("the Azure CLI (`{}`) isn't installed: see {INSTALL_URL}", self.bin);
            }
            bail!(
                "the Azure CLI isn't signed in (or the subscription isn't usable): run `az login`, then retry. {e:#}"
            );
        }
        Ok(())
    }

    // Key Vault only allows letters, digits and hyphens, so STRIPE_KEY is stored as stripe-key.
    fn name_for(&self, wanted: &str) -> String {
        let valid = (1..=127).contains(&wanted.len()) && wanted.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if valid { wanted.to_string() } else { suggest(wanted) }
    }

    fn put(&self, name: &str, value: &str) -> Result<String> {
        // The value goes in on stdin as a one-line file: `--value` would expose it in argv.
        let cmd = self.az(&[
            "keyvault",
            "secret",
            "set",
            "--vault-name",
            &self.opts.vault,
            "--name",
            name,
            "--file",
            "/dev/stdin",
            "--encoding",
            "utf-8",
            "--query",
            "id",
            "-o",
            "tsv",
            "--only-show-errors",
        ]);
        let out = super::run(cmd, Some(value), Some(value))?;
        let id = out.trim();
        if !id.starts_with("https://") {
            bail!("`az keyvault secret set` for {name} didn't return a secret id (got {id:?})");
        }
        Ok(id.to_string())
    }
}

fn check_vault_name(vault: &str) -> Result<()> {
    let ok = (3..=24).contains(&vault.len())
        && vault.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !vault.contains("--");
    if !ok {
        bail!(
            "Key Vault name {vault:?} is invalid: set destination.store.vault to the vault's name \
             (3-24 letters, digits and single hyphens), not its URL"
        );
    }
    Ok(())
}

/// Key Vault secret names: 1-127 characters, letters, digits and hyphens only; case-insensitive.
fn check_secret_names(names: &[&str]) -> Result<()> {
    let mut problems = Vec::new();
    for name in names {
        let valid = (1..=127).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !valid {
            problems.push(format!(
                "{name:?} isn't a valid Key Vault secret name (1-127 letters, digits and hyphens): \
                 set `\"secret\": \"{}\"` on that field",
                suggest(name)
            ));
        }
    }
    let mut seen: HashMap<String, &str> = HashMap::new();
    for name in names {
        if let Some(other) = seen.insert(name.to_ascii_lowercase(), name)
            && other != *name
        {
            problems.push(format!(
                "{other:?} and {name:?} are the same Key Vault secret (names ignore case): rename one with `secret`"
            ));
        }
    }
    if !problems.is_empty() {
        bail!("{}", problems.join("\n"));
    }
    Ok(())
}

fn suggest(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).take(127).collect();
    let s = s.trim_matches('-');
    if s.is_empty() { "my-secret".into() } else { s.to_string() }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::testing::{call, calls, fake_cli, temp_dir};
    use super::*;
    use std::path::Path;

    const SECRET: &str = "s3cr3t-value";
    const ID: &str = "https://acme-vault.vault.azure.net/secrets/stripe-secret-key/0123456789abcdef0123456789abcdef";

    fn store(bin: &Path, subscription: Option<&str>) -> KeyVault {
        KeyVault {
            bin: bin.display().to_string(),
            opts: Options { vault: "acme-vault".into(), subscription: subscription.map(String::from) },
        }
    }

    #[test]
    fn options_reject_unknown_fields() {
        assert!(serde_json::from_str::<Options>(r#"{"vault":"v1x","region":"x"}"#).is_err());
        assert!(serde_json::from_str::<Options>(r#"{}"#).is_err());
        let o: Options = serde_json::from_str(r#"{"vault":"acme-vault"}"#).unwrap();
        assert_eq!(backend(&o).describe(), ("Azure Key Vault".into(), "vault acme-vault".into()));
    }

    #[test]
    fn put_sends_the_secret_on_stdin_only() {
        let dir = temp_dir("azure-put");
        let az = fake_cli(&dir, "az", &format!("echo '{ID}'"));
        let reference = store(&az, None).put("stripe-secret-key", SECRET).unwrap();
        assert_eq!(reference, ID);
        assert_eq!(calls(&dir), 1);
        let (args, stdin) = call(&dir, 1);
        assert_eq!(
            args,
            [
                "keyvault",
                "secret",
                "set",
                "--vault-name",
                "acme-vault",
                "--name",
                "stripe-secret-key",
                "--file",
                "/dev/stdin",
                "--encoding",
                "utf-8",
                "--query",
                "id",
                "-o",
                "tsv",
                "--only-show-errors"
            ]
        );
        assert_eq!(stdin, SECRET, "no trailing newline");
        assert!(args.iter().all(|a| !a.contains(SECRET)));
    }

    #[test]
    fn subscription_is_passed_when_set() {
        let dir = temp_dir("azure-sub");
        let az = fake_cli(&dir, "az", &format!("echo '{ID}'"));
        store(&az, Some("Prod")).put("k", SECRET).unwrap();
        let (args, _) = call(&dir, 1);
        assert_eq!(&args[args.len() - 2..], ["--subscription", "Prod"]);
    }

    #[test]
    fn put_error_does_not_leak_the_secret() {
        let dir = temp_dir("azure-leak");
        let az = fake_cli(&dir, "az", &format!("echo \"Forbidden: could not store {SECRET}\" >&2; exit 1"));
        let err = format!("{:#}", store(&az, None).put("k", SECRET).unwrap_err());
        assert!(err.contains("Forbidden") && err.contains("[redacted]") && !err.contains(SECRET), "{err}");
    }

    #[test]
    fn put_rejects_output_that_isnt_an_id() {
        let dir = temp_dir("azure-badout");
        let az = fake_cli(&dir, "az", "echo");
        assert!(store(&az, None).put("k", SECRET).is_err());
    }

    #[test]
    fn preflight_passes_when_signed_in_and_creates_nothing() {
        let dir = temp_dir("azure-pre-ok");
        let az = fake_cli(&dir, "az", "exit 0");
        store(&az, Some("Prod")).preflight(&["stripe-secret-key", "Other"]).unwrap();
        assert_eq!(calls(&dir), 1);
        let (args, stdin) = call(&dir, 1);
        assert_eq!(args, ["account", "show", "--only-show-errors", "-o", "none", "--subscription", "Prod"]);
        assert_eq!(stdin, "");
    }

    #[test]
    fn preflight_reports_missing_cli() {
        let missing = Path::new("/nonexistent/az");
        let err = store(missing, None).preflight(&["k"]).unwrap_err().to_string();
        assert!(err.contains("isn't installed") && err.contains(INSTALL_URL), "{err}");
    }

    #[test]
    fn preflight_reports_not_signed_in() {
        let dir = temp_dir("azure-pre-login");
        let az = fake_cli(&dir, "az", "echo 'Please run az login' >&2; exit 1");
        let err = store(&az, None).preflight(&["k"]).unwrap_err().to_string();
        assert!(err.contains("az login"), "{err}");
    }

    #[test]
    fn preflight_rejects_invalid_names_with_a_suggestion() {
        let dir = temp_dir("azure-pre-names");
        let az = fake_cli(&dir, "az", "exit 0");
        let err = store(&az, None).preflight(&["STRIPE_SECRET_KEY", "fine-name", "a.b"]).unwrap_err().to_string();
        assert!(err.contains(r#"set `"secret": "stripe-secret-key"` on that field"#), "{err}");
        assert!(err.contains(r#""secret": "a-b""#) && !err.contains("fine-name"), "{err}");
        assert_eq!(calls(&dir), 0, "names are checked before touching az");
    }

    #[test]
    fn preflight_rejects_empty_overlong_and_case_clashing_names() {
        let dir = temp_dir("azure-pre-edge");
        let az = fake_cli(&dir, "az", "exit 0");
        let long = "a".repeat(128);
        assert!(store(&az, None).preflight(&[""]).is_err());
        assert!(store(&az, None).preflight(&[&long]).is_err());
        assert!(store(&az, None).preflight(&[&"a".repeat(127)]).is_ok());
        let err = store(&az, None).preflight(&["Api-Key", "api-key"]).unwrap_err().to_string();
        assert!(err.contains("ignore case"), "{err}");
    }

    #[test]
    fn preflight_rejects_a_vault_url() {
        let dir = temp_dir("azure-pre-vault");
        let az = fake_cli(&dir, "az", "exit 0");
        let mut s = store(&az, None);
        s.opts.vault = "https://acme-vault.vault.azure.net/".into();
        assert!(s.preflight(&["k"]).unwrap_err().to_string().contains("not its URL"));
    }

    #[test]
    fn suggestions_are_valid_names() {
        assert_eq!(suggest("STRIPE_SECRET_KEY"), "stripe-secret-key");
        assert_eq!(suggest("__"), "my-secret");
        assert!(check_secret_names(&[&suggest("DB.PASSWORD 2")]).is_ok());
    }
}
