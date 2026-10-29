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
    pub path: PathBuf,
    #[serde(default)]
    pub format: Option<Format>,
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
}

impl Spec {
    pub fn parse(text: &str) -> Result<Spec> {
        let spec: Spec = serde_json::from_str(text).context("spec is not valid agentikeys JSON")?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn format(&self) -> Result<Format> {
        match self.destination.format.or_else(|| Format::infer(&self.destination.path)) {
            Some(f) => Ok(f),
            None => bail!(
                "can't infer a format from {:?}; set destination.format to dotenv, json or toml",
                self.destination.path
            ),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() {
            bail!("title is empty");
        }
        if self.fields.is_empty() || self.fields.len() > 20 {
            bail!("fields must have between 1 and 20 entries");
        }
        let format = self.format()?;
        let mut seen = HashSet::new();
        for f in &self.fields {
            if !seen.insert(f.key_path()) {
                bail!("duplicate key {:?}", f.key_path());
            }
            if format == Format::Dotenv {
                if f.key.is_some() {
                    bail!("field {:?}: `key` is only for json/toml; dotenv uses `name`", f.name);
                }
                if !is_env_name(&f.name) {
                    bail!("field {:?}: dotenv names must match [A-Za-z_][A-Za-z0-9_]*", f.name);
                }
            } else if f.key_path().split('.').any(str::is_empty) {
                bail!("field {:?}: key path {:?} has an empty segment", f.name, f.key_path());
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
