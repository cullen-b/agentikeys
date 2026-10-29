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
2. agentikeys checks everything first: the spec, that the destination parses, and that the file
   isn't tracked by git. Only then does it start a one-shot server (random port, random 256-bit
   token in the URL) reachable the way the user picked (see [Reaching the user](#reaching-the-user)),
   and prints the link.
3. The page is built from a fixed template; agent-supplied text is only ever inserted as text. It
   shows where the values will be saved and whether the file will be gitignored.
4. On submit, agentikeys adds the file to `.gitignore` if needed, upserts the values (other entries
   and comments stay), writes atomically with `0600` permissions, and shuts the server down.
5. The agent gets a receipt on stdout.

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

- `destination.format` is `dotenv`, `json` or `toml`; it's inferred from the file name when omitted.
- For JSON/TOML, a field's `key` sets a dotted path (`"key": "database.password"`); it defaults to `name`.
- `pattern` is a hint: a mismatch warns the user, who can still save.

Receipt (stdout, one JSON line):

```json
{"status":"saved","path":"/home/me/app/.env.local","format":"dotenv","keys":["STRIPE_SECRET_KEY"],"gitignore":"added"}
```

| status | exit code |
|---|---|
| `saved` | 0 |
| `error` (see `error`) | 1 |
| `cancelled` | 2 |
| `timed_out` (default 600 s, `--timeout`) | 3 |
| `pending` from `wait` (still waiting) | 4 |

`request` itself exits 0 with `pending` once the link is ready. A pending receipt may carry a
`note` for the user (e.g. the tunnel's Tailscale suggestion).

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

## Instructions for agents

Paste into `AGENTS.md`, `CLAUDE.md` or your harness's equivalent:

```md
## Secrets
Never ask the user to paste an API key, token or password into chat. Instead run
`agentikeys request` with a JSON spec on stdin describing the fields and the destination file. Post
the returned `url` in chat as a link for the user to open in their own browser, along with any
`note`. Never open the link yourself, including in a built-in or automated browser. Then run
`agentikeys wait <id>` until the status is no longer `pending`. Never read the destination file
back or print its contents. If the user isn't at this machine, use `--via tailscale` (or
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
- v2: 1Password, Vault, GCP / AWS Secret Manager, Doppler, and user-defined integrations, writing
  references (e.g. `op://...`) into files instead of raw values.
