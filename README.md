# agentikeys

Get secrets from a human into the right place without them ever passing through an AI agent's chat.

## The problem

When an agent needs an API key, the usual flow is "paste it here". The key then lives in the chat
transcript, the provider's logs, synced chat history, and sometimes a commit. agentikeys replaces the
paste with a one-shot page: the agent describes *what* it needs and *where* it goes, agentikeys
gives it a link to post in chat (and opens it in the browser when the user is at the machine), the
user enters the values, and agentikeys writes them. The agent only gets back a receipt.

## How it works

1. The agent runs `agentikeys request` with a JSON spec (no secret values in it).
2. agentikeys checks the spec and the suggested file (it must parse and not be tracked by git), and
   looks for secret stores this computer is signed in to (Google Cloud, AWS, Azure). Then it starts a
   one-shot server (random port, random 256-bit token in the URL) reachable the way the user picked
   (see [Reaching the user](#reaching-the-user)), and prints the link.
3. The page is built from a fixed template; agent-supplied text is only ever inserted as text. Under
   **Save to**, the user ticks every place the values should go: the suggested file and any detected
   store (with a project or vault to pick). The agent's suggestions start ticked; stores that need
   attention show how to fix them.
4. On submit, agentikeys checks every ticked place first, then saves to each. Files are gitignored if
   needed, upserted (other entries and comments stay) and written atomically with `0600`
   permissions. If a place fails, the page says which and stays open to retry; places already saved
   are marked and not written again.
5. The agent gets a receipt on stdout listing every place the values went.

## Install

```sh
cargo install --path .
```

## Usage

```sh
agentikeys request < spec.json        # prints {"status":"pending","id":…,"url":…} straight away
agentikeys wait <id> --timeout 60     # blocks until saved/cancelled/timed out, or reports pending
agentikeys request --foreground spec.json   # or block until the user is done
```

Spec:

```json
{
  "title": "Stripe test keys",
  "reason": "The checkout service needs these to call the Stripe API in test mode.",
  "fields": [
    {"name": "STRIPE_SECRET_KEY", "label": "Secret key", "pattern": "^sk_test_",
     "placeholder": "sk_test_…", "help_url": "https://dashboard.stripe.com/test/apikeys"},
    {"name": "STRIPE_WEBHOOK_SECRET", "label": "Webhook signing secret", "optional": true}
  ],
  "destination": {"path": ".env.local"}
}
```

- `destination` is a suggestion: `path` offers (and preticks) a file, `store` preticks a secret store
  (see [Secret stores](#secret-stores)). Both are optional; the user decides on the page.
- `destination.format` is `dotenv`, `json` or `toml`; it's inferred from the file name when omitted.
- For JSON/TOML, a field's `key` sets a dotted path (`"key": "database.password"`); it defaults to `name`.
- `pattern` is a hint: a mismatch warns the user, who can still save.

Receipt (stdout, one JSON line):

```json
{"status":"saved","keys":["STRIPE_SECRET_KEY"],"saved_to":[
  {"kind":"file","location":"/home/me/app/.env.local","keys":["STRIPE_SECRET_KEY"],"gitignore":"added"},
  {"kind":"gcp_secret_manager","location":"project acme-prod","keys":["STRIPE_SECRET_KEY"],
   "references":{"STRIPE_SECRET_KEY":"projects/123/secrets/STRIPE_SECRET_KEY/versions/1"}}]}
```

| status | exit code |
|---|---|
| `saved` | 0 |
| `error` (see `error`) | 1 |
| `cancelled` | 2 |
| `timed_out` (default 600 s, `--timeout`) | 3 |
| `pending` from `wait` (still waiting) | 4 |

`request` itself exits 0 with `pending` once the link is ready. A pending receipt may carry a
`note` for the user (e.g. the tunnel's Tailscale suggestion). `cancelled` and `timed_out` receipts
still list `saved_to` if some places were saved before the user stopped.

## Reaching the user

| `--via` | Page is served on | Use when |
|---|---|---|
| `local` | `127.0.0.1`, opened in this machine's browser | you're at the machine |
| `tailscale` | this machine's Tailscale address only | you're on another device in your tailnet |
| `tunnel` | a temporary `*.trycloudflare.com` quick tunnel (needs `cloudflared`, no account) | anywhere else |
| `auto` (default) | `local`, or over SSH `tailscale` if it's running, else `tunnel` | |

Set a default once with `agentikeys config via tailscale` (or `AGENTIKEYS_VIA`); `--via` overrides
both. In tunnel mode the link's `#k=` fragment carries a one-time P-256 public key that browsers never
send to servers; the page encrypts the values (ECDH → HKDF-SHA256 → AES-256-GCM, bound to the link)
before sending, so Cloudflare relays only ciphertext. Cloudflare does serve the page itself, so tunnel
mode trusts Cloudflare not to alter it; Tailscale avoids that. The tunnel closes when the request ends.

## Secret stores

The page offers every secret store whose CLI is installed here; stores you're signed in to can be
ticked, the rest show how to sign in. An agent can pretick one, with options, via
`destination.store`:

```json
{
  "title": "Stripe keys",
  "fields": [{"name": "STRIPE_SECRET_KEY"}],
  "destination": {"store": {"type": "gcp_secret_manager", "project": "acme-prod"}}
}
```

| `store.type` | Uses | Docs |
|---|---|---|
| `gcp_secret_manager` | `gcloud` | [docs/stores/gcp.md](docs/stores/gcp.md) |
| `aws_secrets_manager`, `aws_parameter_store` | `aws` | [docs/stores/aws.md](docs/stores/aws.md) |
| `azure_key_vault` | `az` | [docs/stores/azure.md](docs/stores/azure.md) |

Each store uses its official CLI and the login you already have; detection only runs read-only
commands. Values reach the CLI only on stdin, never as a command-line argument. Before saving,
agentikeys re-checks every ticked store (signed in, names valid, store usable), so a bad pick
fails before anything is written. A field's `secret` sets its name in stores (it defaults to
`name`); Azure, which only allows letters, digits and hyphens, stores `STRIPE_KEY` as `stripe-key`
and the page says so. Each store's entry in `saved_to` has `references` (resource names, ARNs or
URLs) an app can read the secret back by. Stores are macOS/Linux only for now.

## Instructions for agents

Paste into `AGENTS.md`, `CLAUDE.md` or your harness's equivalent:

```md
## Secrets
Never ask the user to paste an API key, token or password into chat. Instead run
`agentikeys request` with a JSON spec on stdin describing the fields, and suggest a destination:
the file the app reads its config from and/or the secret store the project uses. The user picks
the final places on the page; follow the receipt's `saved_to` (e.g. its `references`). Post
the returned `url` in chat as a link for the user to open in their own browser, along with any
`note`. Never open the link yourself, including in a built-in or automated browser. Then run
`agentikeys wait <id>` until the status is no longer `pending`. Never read the destination file
or the stored secrets back, or print their contents. If the user isn't at this machine, use `--via tailscale` (or
`--via tunnel` if they don't use Tailscale).
```

## Threat model

Protects against: secrets in chat transcripts, provider logs, chat history, screen shares, and
accidental commits (it refuses git-tracked destinations and gitignores new ones).

Does not, by itself, protect against: an agent with shell access running `cat .env.local`. That is
what the vault integrations below are for.

## Roadmap

- v1: OS keychain backend, `agentikeys run -- <cmd>` injecting env vars at launch with output
  redaction, MCP server.
- v2: 1Password, Vault/OpenBao, GitHub Actions secrets, Doppler, Infisical, and user-defined
  integrations; saved default destinations so agents don't have to ask.
