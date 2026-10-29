### Azure Key Vault

Stores each field as a secret in an Azure Key Vault using the `az` CLI and your existing `az login`.
Values reach `az` on stdin only (never argv, env vars or files).

```json
{
  "title": "Stripe test keys",
  "reason": "The checkout service needs these to call the Stripe API in test mode.",
  "fields": [
    {"name": "STRIPE_SECRET_KEY", "secret": "stripe-secret-key", "label": "Secret key"},
    {"name": "STRIPE_WEBHOOK_SECRET", "secret": "stripe-webhook-secret", "label": "Webhook signing secret"}
  ],
  "destination": {"store": {"type": "azure_key_vault", "vault": "acme-vault"}}
}
```

Key Vault secret names allow only letters, digits and hyphens (1-127 characters, case-insensitive). A
field without a valid `secret` name is stored under a converted one (`STRIPE_SECRET_KEY` becomes
`stripe-secret-key`); the page shows the converted names next to Key Vault, and the receipt's
references use them. Names that collide ignoring case are rejected before anything is saved.
`destination.store` only preticks this store on the page; the user can untick it or add other
places. The receipt's `saved_to` entry for it carries the references below.

| Option | Required | Meaning |
|---|---|---|
| `vault` | yes | Key Vault name (3-24 letters, digits, hyphens), not its URL |
| `subscription` | no | Subscription name or ID; defaults to the one `az` has selected (`az account show`) |

**On the page**, Key Vault is offered when `az` is installed ([install](https://learn.microsoft.com/cli/azure/install-azure-cli))
and signed in (`az account show`, else run `az login`), with the vaults `az keyvault list` finds to pick
from. Listing needs read access to the subscription; without it, a vault the agent suggests in
`destination.store.vault` is still offered. Before saving, agentikeys re-checks sign-in and names. It
doesn't contact the vault until it saves: a reachability check needs read permissions a write-only
identity may not have, so a wrong vault name or missing permission surfaces as a save error on the
page, which stays open to retry.

**Permissions** for the signed-in identity: the RBAC role *Key Vault Secrets Officer* on the vault (or
a vault using access policies: a policy with the *Set* secret permission). It also needs network
access to the vault if a firewall is enabled.

**Reference** returned and written for each field: the versioned secret id, e.g.
`https://acme-vault.vault.azure.net/secrets/stripe-secret-key/0123456789abcdef0123456789abcdef`.
Saving again adds a new version (`az keyvault secret set` never overwrites), so old references stay valid.

**Reading it back** (needs the *Key Vault Secrets User* role or *Get* permission):

```sh
az keyvault secret show --vault-name acme-vault --name stripe-secret-key --query value -o tsv
```

**How the value is passed:** `az keyvault secret set --file /dev/stdin --encoding utf-8 …` (documented
[parameters](https://learn.microsoft.com/cli/azure/keyvault/secret#az-keyvault-secret-set); `--value`
would put the secret in the process list). Notes:
- Unix only for now (macOS, Linux): Windows has no `/dev/stdin`.
- `az` reads the file as text without trimming, so no trailing newline is added; but Python's text mode
  turns `\r\n` inside a value into `\n`, and non-ASCII values depend on a UTF-8 locale.
- `az` tags the secret `file-encoding=utf-8`.
