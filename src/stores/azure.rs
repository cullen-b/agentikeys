//! Azure Key Vault via `az`. Placeholder: being implemented.

use super::SecretStore;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {}

pub fn backend(_opts: &Options) -> Box<dyn SecretStore> {
    Box::new(Unimplemented)
}

struct Unimplemented;

impl SecretStore for Unimplemented {
    fn describe(&self) -> (String, String) {
        ("Azure Key Vault".into(), String::new())
    }
    fn preflight(&self, _names: &[&str]) -> Result<()> {
        bail!("Azure Key Vault support isn't finished yet")
    }
    fn put(&self, _name: &str, _value: &str) -> Result<String> {
        bail!("Azure Key Vault support isn't finished yet")
    }
}
