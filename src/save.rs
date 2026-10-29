//! The places offered on the page, and saving the values to every place the user ticked.

use crate::spec::Spec;
use crate::store::{self, FilePlan, GitStatus};
use crate::stores::{Offer, SecretStore, StoreSpec};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const FILE: &str = "file";

/// Field name -> value, overwritten with zeros when dropped. Best effort: copies made inside serde,
/// the CLIs and the OS (pipe buffers) are out of reach.
#[derive(Default, Deserialize)]
#[serde(transparent)]
pub struct Secrets(BTreeMap<String, String>);

impl std::ops::Deref for Secrets {
    type Target = BTreeMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Secrets {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for Secrets {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.values_mut().for_each(Zeroize::zeroize);
    }
}

/// Everything the user can pick from, worked out before the page opens.
pub struct Places {
    pub file: Option<FilePlan>,
    pub offers: Vec<Offer>,
}

/// One ticked place in a submission.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pick {
    pub id: String,
    #[serde(default)]
    pub choice: Option<String>,
}

/// One place the values were saved to; goes in the receipt. Never contains values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedTo {
    /// "file", or a store type such as "gcp_secret_manager".
    pub kind: String,
    /// File path, or the store's location (e.g. "project acme-prod").
    pub location: String,
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gitignore: Option<String>,
    /// Field name -> where the store keeps it (not secret).
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub references: BTreeMap<String, String>,
}

pub struct Outcome {
    pub saved: Vec<SavedTo>,
    /// One message per place that failed; places listed in `saved` succeeded.
    pub failed: Vec<String>,
}

impl Places {
    pub fn new(spec: &Spec, file: Option<FilePlan>, mut offers: Vec<Offer>) -> Result<Places> {
        // Keep the agent's suggested store visible even when it isn't usable here.
        if let Some(s) = &spec.destination.store
            && !offers.iter().any(|o| o.kind == s.kind())
        {
            let service = s.backend().describe().0;
            offers.push(Offer::unavailable(s.kind(), service, "Its command-line tool isn't installed on this computer"));
        }
        let places = Places { file, offers };
        if places.file.is_none() && places.offers.iter().all(|o| places.view(spec, o).problem.is_some()) {
            bail!(
                "there's nowhere to save these: suggest a file in destination.path, or have the user sign in \
                 to a secret store's CLI (gcloud, aws or az)"
            );
        }
        Ok(places)
    }

    /// An offer's choices, preselected choice and problem, folding in the agent's suggestion.
    fn view(&self, spec: &Spec, o: &Offer) -> View {
        let suggested = spec.destination.store.as_ref().filter(|s| s.kind() == o.kind);
        let mut choices = o.choices.clone();
        let mut default = o.default_choice.clone();
        if let Some(c) = suggested.and_then(|s| s.choice()).filter(|c| !c.is_empty()) {
            if !choices.contains(&c) && o.choice_label.is_some() {
                choices.insert(0, c.clone());
            }
            default = Some(c);
        }
        let problem = o.problem.clone().or_else(|| {
            let label = o.choice_label?;
            choices.is_empty().then(|| format!("No {}s found for this account", label.to_lowercase()))
        });
        View { selected: suggested.is_some() && problem.is_none(), choices, default, problem }
    }

    /// The places as the page shows them, with the agent's suggestions preselected.
    pub fn for_page(&self, spec: &Spec, tilde: impl Fn(&str) -> String) -> Vec<Value> {
        let mut out = Vec::new();
        if let Some(f) = &self.file {
            let git = match f.git {
                GitStatus::AlreadyIgnored => "Already ignored by git.",
                GitStatus::WillIgnore => "Will be added to .gitignore so it is never committed.",
                GitStatus::NotInRepo => "Not inside a git repository.",
            };
            out.push(json!({
                "id": FILE, "service": format!("A {} file on this computer", f.format.as_str()),
                "location": tilde(&f.path.display().to_string()), "detail": git, "selected": true,
            }));
        }
        let suggested = spec.destination.store.as_ref();
        for o in &self.offers {
            let View { selected, choices, default, problem } = self.view(spec, o);
            let names = default
                .as_deref()
                .or(if o.choice_label.is_none() { Some("") } else { None })
                .and_then(|c| StoreSpec::chosen(o.kind, (!c.is_empty()).then_some(c), suggested))
                .map(|s| renamed(spec, s.backend().as_ref()));
            out.push(json!({
                "id": o.kind, "service": o.service, "detail": o.detail, "problem": problem,
                "choice_label": o.choice_label, "choices": choices, "default_choice": default,
                "selected": selected, "names": names.filter(|n| !n.is_empty()),
            }));
        }
        out
    }

    /// Saves `values` (field name -> value) to every pick. Checks all picks before saving anything.
    pub fn save(&self, spec: &Spec, picks: &[Pick], values: &BTreeMap<String, String>) -> Result<Outcome> {
        if picks.is_empty() {
            bail!("Pick at least one place to save to.");
        }
        let mut stores: Vec<(&Pick, Box<dyn SecretStore>)> = Vec::new();
        for pick in picks {
            if pick.id == FILE {
                self.file.as_ref().ok_or_else(|| anyhow!("There's no file to save to."))?;
                continue;
            }
            let offer = self.offers.iter().find(|o| o.kind == pick.id).ok_or_else(|| anyhow!("Unknown place {}.", pick.id))?;
            let view = self.view(spec, offer);
            if let Some(problem) = &view.problem {
                bail!("{}: {problem}", offer.service);
            }
            if offer.choice_label.is_some() && !pick.choice.as_ref().is_some_and(|c| view.choices.contains(c)) {
                bail!("Pick one of the listed {}s for {}.", offer.choice_label.unwrap_or("target").to_lowercase(), offer.service);
            }
            let chosen = StoreSpec::chosen(&pick.id, pick.choice.as_deref(), spec.destination.store.as_ref())
                .ok_or_else(|| anyhow!("Unknown place {}.", pick.id))?;
            let backend = chosen.backend();
            let names: Vec<String> = spec.fields.iter().map(|f| backend.name_for(f.secret_name())).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            backend.preflight(&names).map_err(|e| anyhow!("{}: {e:#}", offer.service))?;
            stores.push((pick, backend));
        }

        let keys: Vec<String> = spec.fields.iter().filter(|f| values.contains_key(&f.name)).map(|f| f.name.clone()).collect();
        let mut outcome = Outcome { saved: Vec::new(), failed: Vec::new() };
        for (pick, backend) in &stores {
            let (service, location) = backend.describe();
            let mut references = BTreeMap::new();
            let result = spec.fields.iter().filter(|f| values.contains_key(&f.name)).try_for_each(|f| {
                let reference = backend.put(&backend.name_for(f.secret_name()), &values[&f.name])?;
                references.insert(f.name.clone(), reference);
                anyhow::Ok(())
            });
            match result {
                Ok(()) => outcome.saved.push(SavedTo {
                    kind: pick.id.clone(),
                    location,
                    keys: keys.clone(),
                    gitignore: None,
                    references,
                }),
                Err(e) => outcome.failed.push(format!("{service} ({location}): {e:#}")),
            }
        }
        if let Some(file) = picks.iter().any(|p| p.id == FILE).then_some(self.file.as_ref()).flatten() {
            match store::write_values(spec, file, values) {
                Ok(()) => outcome.saved.push(SavedTo {
                    kind: FILE.into(),
                    location: file.path.display().to_string(),
                    keys,
                    gitignore: Some(
                        match file.git {
                            GitStatus::NotInRepo => "not_in_repo",
                            GitStatus::AlreadyIgnored => "already_ignored",
                            GitStatus::WillIgnore => "added",
                        }
                        .into(),
                    ),
                    references: BTreeMap::new(),
                }),
                Err(e) => outcome.failed.push(format!("{}: {e:#}", file.path.display())),
            }
        }
        Ok(outcome)
    }
}

struct View {
    selected: bool,
    choices: Vec<String>,
    default: Option<String>,
    problem: Option<String>,
}

/// "Saved as ..." when the store renames fields to fit its rules.
fn renamed(spec: &Spec, backend: &dyn SecretStore) -> String {
    let changed: Vec<String> = spec
        .fields
        .iter()
        .map(|f| backend.name_for(f.secret_name()))
        .zip(spec.fields.iter())
        .filter(|(n, f)| n != f.secret_name())
        .map(|(n, _)| n)
        .collect();
    if changed.is_empty() { String::new() } else { format!("Saved as {}", changed.join(", ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(destination: &str) -> Spec {
        Spec::parse(&format!(r#"{{"title":"t","fields":[{{"name":"STRIPE_KEY"}}],"destination":{destination}}}"#)).unwrap()
    }

    fn vault_offer(choices: &[&str]) -> Offer {
        Offer {
            kind: "azure_key_vault",
            service: "Azure Key Vault".into(),
            detail: String::new(),
            problem: None,
            choice_label: Some("Vault"),
            choices: choices.iter().map(|c| c.to_string()).collect(),
            default_choice: choices.first().map(|c| c.to_string()),
        }
    }

    #[test]
    fn suggested_store_is_preselected_and_counts_as_a_choice() {
        let spec = spec(r#"{"store":{"type":"azure_key_vault","vault":"team-vault"}}"#);
        let places = Places::new(&spec, None, vec![vault_offer(&[])]).unwrap();
        let page = places.for_page(&spec, |p| p.to_string());
        assert_eq!(page[0]["selected"], true);
        assert_eq!(page[0]["choices"], json!(["team-vault"]));
        assert_eq!(page[0]["names"], "Saved as stripe-key");
    }

    #[test]
    fn nothing_to_pick_disables_the_store() {
        let spec = spec("{}");
        assert!(Places::new(&spec, None, vec![vault_offer(&[])]).is_err(), "nowhere usable to save");
        let places = Places::new(&spec, None, vec![vault_offer(&[]), vault_offer(&["v1"])]).unwrap();
        let page = places.for_page(&spec, |p| p.to_string());
        assert_eq!(page[0]["problem"], "No vaults found for this account");
        assert_eq!(page[0]["selected"], false);
    }

    #[test]
    fn bad_picks_fail_before_anything_runs() {
        let spec = spec("{}");
        let places = Places::new(&spec, None, vec![vault_offer(&["v1"])]).unwrap();
        let values = BTreeMap::from([("STRIPE_KEY".to_string(), "x".to_string())]);
        let pick = |id: &str, choice: Option<&str>| Pick { id: id.into(), choice: choice.map(String::from) };
        let err = |picks: &[Pick]| places.save(&spec, picks, &values).err().unwrap().to_string();
        assert!(err(&[]).contains("at least one"));
        assert!(err(&[pick("file", None)]).contains("no file"));
        assert!(err(&[pick("aws_secrets_manager", None)]).contains("Unknown place"));
        assert!(err(&[pick("azure_key_vault", Some("not-offered"))]).contains("listed vaults"));
    }
}
