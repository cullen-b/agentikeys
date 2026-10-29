# agentikeys

Get secrets from a human into the right place without them ever passing through an AI agent's chat.

## The problem

When an agent needs an API key, the usual flow is "paste it here". The key then lives in the chat
transcript, the provider's logs, synced chat history, and sometimes a commit. agentikeys replaces the
paste with a local, one-shot form: the agent describes *what* it needs and *where* it goes, the user
types the values into a local browser page, and agentikeys writes them. The agent only gets back a
receipt.

## Flow (v0, local only)

1. Agent runs `agentikeys request` with a declarative spec:
   ```json
   {
     "title": "Stripe test keys",
     "reason": "The checkout service needs these to call the Stripe API.",
     "fields": [
       {"name": "STRIPE_SECRET_KEY", "label": "Secret key", "pattern": "^sk_test_", "help_url": "https://dashboard.stripe.com/test/apikeys"},
       {"name": "STRIPE_WEBHOOK_SECRET", "label": "Webhook signing secret", "optional": true}
     ],
     "destination": {"path": ".env.local", "format": "dotenv"}
   }
   ```
2. agentikeys starts a server on `127.0.0.1` (random port, single-use token in the URL), opens the
   browser, and shows the form rendered from a fixed trusted template — never agent-written HTML.
   The page shows the destination path so the user can see where the values will go.
3. The user submits; agentikeys upserts the values into the destination (keeping other entries and
   comments), makes sure the file is gitignored, sets `0600` permissions, then shuts down.
4. The agent receives only a receipt: `{"status": "written", "path": ".env.local", "keys": ["STRIPE_SECRET_KEY"]}`.

## Interfaces

- **CLI** first: every harness can run a shell command, so this works everywhere on day one.
- **MCP server** wrapping the same core, for harnesses that prefer tools.
- **Agent instructions snippet** (`AGENTS.md` / `CLAUDE.md` / skill) telling agents to call
  agentikeys instead of asking for a paste.

## Threat model (be honest about it)

Protects against: secrets in chat transcripts, provider logs, chat history, screen shares, and
accidental commits.

Does not, by itself, protect against: an agent with shell access running `cat .env.local`. Later
mitigations: keychain storage plus `agentikeys run -- <cmd>` injecting env vars at launch with
output redaction, and harness hooks that block reading secret files.

## Roadmap

- v0: CLI, browser form, dotenv / JSON / YAML / TOML destinations.
- v1: OS keychain backend, `agentikeys run`, MCP server, output redaction.
- v2: 1Password, Vault, GCP / AWS Secret Manager, Doppler — write references (e.g. `op://...`)
  into files instead of raw values.
