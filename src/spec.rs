use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What the agent asks for. Contains no secret values.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub title: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub fields: Vec<Field>,
    pub destination: Destination,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Dotted path inside JSON/TOML files; defaults to `name`.
    #[serde(default)]
    pub key: Option<String>,
    /// Secret name in `destination.store`; defaults to `name`.
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
    /// JavaScript regex the value should match; a typing aid, not a security check.
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub help_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    /// File that receives the values, or only references to them when `store` is set.
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub format: Option<Format>,
    /// Cloud secret store that receives the values.
    #[serde(default)]
    pub store: Option<crate::stores::StoreSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Dotenv,
    Json,
    Toml,
}

impl Format {
    pub fn infer(path: &Path) -> Option<Format> {
        let name = path.file_name()?.to_str()?;
        if name.starts_with(".env") || name.ends_with(".env") {
            return Some(Format::Dotenv);
        }
        match path.extension()?.to_str()? {
            "json" => Some(Format::Json),
            "toml" => Some(Format::Toml),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Dotenv => "dotenv",
            Format::Json => "json",
            Format::Toml => "toml",
        }
    }
}

impl Field {
    pub fn key_path(&self) -> &str {
        self.key.as_deref().unwrap_or(&self.name)
    }

    pub fn secret_name(&self) -> &str {
        self.secret.as_deref().unwrap_or(&self.name)
    }
}

impl Spec {
    pub fn parse(text: &str) -> Result<Spec> {
        let spec: Spec = serde_json::from_str(text).context("spec is not valid agentikeys JSON")?;
        spec.validate()?;
        Ok(spec)
    }

    /// Format of `destination.path`, or None when there's no file.
    pub fn file_format(&self) -> Result<Option<Format>> {
        let Some(path) = &self.destination.path else { return Ok(None) };
        match self.destination.format.or_else(|| Format::infer(path)) {
            Some(f) => Ok(Some(f)),
            None => bail!("can't infer a format from {path:?}; set destination.format to dotenv, json or toml"),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() {
            bail!("title is empty");
        }
        if self.fields.is_empty() || self.fields.len() > 20 {
            bail!("fields must have between 1 and 20 entries");
        }
        if self.destination.path.is_none() && self.destination.store.is_none() {
            bail!("destination needs a path, a store, or both");
        }
        let format = self.file_format()?;
        let mut seen = HashSet::new();
        let mut seen_secret = HashSet::new();
        for f in &self.fields {
            if !seen.insert(f.key_path()) {
                bail!("duplicate key {:?}", f.key_path());
            }
            if f.secret.is_some() && self.destination.store.is_none() {
                bail!("field {:?}: `secret` only applies with destination.store", f.name);
            }
            if format == Some(Format::Dotenv) {
                if f.key.is_some() {
                    bail!("field {:?}: `key` is only for json/toml; dotenv uses `name`", f.name);
                }
                if !is_env_name(&f.name) {
                    bail!("field {:?}: dotenv names must match [A-Za-z_][A-Za-z0-9_]*", f.name);
                }
            } else if format.is_some() && f.key_path().split('.').any(str::is_empty) {
                bail!("field {:?}: key path {:?} has an empty segment", f.name, f.key_path());
            }
            if self.destination.store.is_some() && !seen_secret.insert(f.secret_name()) {
                bail!("duplicate secret name {:?}", f.secret_name());
            }
            if let Some(url) = &f.help_url
                && !(url.starts_with("https://") || url.starts_with("http://"))
            {
                bail!("field {:?}: help_url must be http(s)", f.name);
            }
        }
        Ok(())
    }
}

fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
