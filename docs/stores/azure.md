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
  "destination": {
    "path": ".env.refs",
    "store": {"type": "azure_key_vault", "vault": "acme-vault"}
  }
}
```

Key Vault secret names allow only letters, digits and hyphens (1-127 characters, case-insensitive), so
`STRIPE_SECRET_KEY` is rejected up front and the error suggests the `secret` name to use. Names are never
renamed silently. With `destination.path` set, the file receives the references (one per field), not values.

| Option | Required | Meaning |
|---|---|---|
| `vault` | yes | Key Vault name (3-24 letters, digits, hyphens), not its URL |
| `subscription` | no | Subscription name or ID; defaults to the one `az` has selected (`az account show`) |

**Before the page opens** agentikeys checks that `az` is installed ([install](https://learn.microsoft.com/cli/azure/install-azure-cli)),
that you're signed in (`az account show`, else run `az login`), and that the names are valid. It creates
nothing and does not contact the vault: a reachability check needs read permissions (list/get) that a
write-only identity may not have, so a wrong vault name or missing permission surfaces when saving.

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
