//! AWS Secrets Manager and Systems Manager Parameter Store via `aws`. Placeholder: being implemented.

use super::SecretStore;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsManagerOptions {}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterStoreOptions {}

pub fn secrets_manager(_opts: &SecretsManagerOptions) -> Box<dyn SecretStore> {
    Box::new(Unimplemented("AWS Secrets Manager"))
}

pub fn parameter_store(_opts: &ParameterStoreOptions) -> Box<dyn SecretStore> {
    Box::new(Unimplemented("AWS Systems Manager Parameter Store"))
}

struct Unimplemented(&'static str);

impl SecretStore for Unimplemented {
    fn describe(&self) -> (String, String) {
        (self.0.into(), String::new())
    }
    fn preflight(&self, _names: &[&str]) -> Result<()> {
        bail!("{} support isn't finished yet", self.0)
    }
    fn put(&self, _name: &str, _value: &str) -> Result<String> {
        bail!("{} support isn't finished yet", self.0)
    }
}
