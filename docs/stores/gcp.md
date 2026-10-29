### Google Cloud Secret Manager

Saves each field as a secret in Google Cloud Secret Manager through the `gcloud` CLI, using the
account you're already signed in with (`gcloud auth login`). Values go to `gcloud` on stdin, never on
the command line.

```json
{
  "title": "Stripe test keys",
  "reason": "The checkout service reads these from Secret Manager.",
  "fields": [
    {"name": "STRIPE_SECRET_KEY", "label": "Secret key"},
    {"name": "STRIPE_WEBHOOK_SECRET", "label": "Webhook signing secret", "secret": "stripe-webhook"}
  ],
  "destination": {
    "store": {"type": "gcp_secret_manager", "project": "acme-prod"},
    "path": ".env.local"
  }
}
```

With `path` set, the file receives the references below instead of the values, so it's safe to commit
or hand to an agent. Omit `path` to store only.

| option | default | meaning |
|---|---|---|
| `project` | `gcloud config get-value project` | Project ID to store secrets in. Fails before the page opens if neither is set. |

- Each field's secret name is its `secret` (or `name`). Secret Manager IDs are 1-255 characters of
  letters, digits, `-` and `_`; an invalid name is rejected before the page opens.
- A new secret is created with automatic replication; an existing secret gets a new version
  (older versions are kept).
- Before the page opens, agentikeys checks that `gcloud` is installed, signed in and has a project, and
  that Secret Manager is usable there (a read-only `gcloud secrets list --limit=1`, which catches a
  disabled API or missing permission). It creates nothing at that point.

**IAM.** The signed-in identity needs, on the project: `roles/secretmanager.admin`, or the narrower
combination of `roles/secretmanager.viewer` (to check whether a secret exists), plus
`roles/secretmanager.secretVersionAdder` to add versions, plus `secretmanager.secrets.create` (in
`roles/secretmanager.admin`, or a custom role) to create new ones. If every secret already exists,
`viewer` + `secretVersionAdder` is enough. The Secret Manager API must be enabled on the project.

**Reference** returned for each field: the version's full resource name,
`projects/<project-number>/secrets/<NAME>/versions/<N>`.

**Reading it back** (needs `roles/secretmanager.secretAccessor`):

```sh
gcloud secrets versions access latest --secret NAME --project P
```
