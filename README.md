# agentikeys

**Let AI coding agents ask for API keys without the keys ever passing through the chat.**

When an agent needs a key, the usual flow is "paste it here". The key then lives in the chat
transcript, the provider's logs, your synced chat history, and sometimes a commit. With agentikeys,
the agent posts a link instead. You open it, paste the key (or upload the key file), tick where it
should be saved (a `.env` file, Google Cloud Secret Manager, AWS, Azure), and the agent only gets
back a receipt saying where it went.

It works with any agent that can run a shell command: Claude Code, Codex, OpenCode, Gemini CLI,
GitHub Copilot, Cursor, Windsurf, Zed, Amp, Cline, Roo Code, Kiro, Goose, Devin and others.

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/cullen-b/agentikeys/main/install.sh | sh
agentikeys setup
```

The install script downloads the release for your machine (macOS or Linux, Intel or ARM), checks its
SHA-256 checksum, and installs it into `~/.local/bin`. Prefer to build it yourself?
`cargo install --git https://github.com/cullen-b/agentikeys`.

`agentikeys setup` teaches every agent harness installed on your machine to use agentikeys, by adding
a short block to each one's user-level instructions file (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
`~/.gemini/GEMINI.md`, and so on). Run `agentikeys setup --dry-run` to see what it would change,
`--uninstall` to remove it, or `--print` to get the text for anywhere else (a project's `AGENTS.md`,
Cursor's Settings → Rules).

That's it. Next time an agent needs a key, it gives you a link.

## What you'll see

- **Your security phrase** in the top corner, e.g. `amber-otter-42`. It's created the first time and
  agents never see it, so a lookalike page can't show it. **If a page asks for keys without your
  phrase, close it.** See it any time with `agentikeys phrase` in your own terminal.
- **The fields the agent asked for.** Type or paste a key; paste multi-line keys (PEM private keys,
  JSON) or upload a key file, and it's kept byte for byte.
- **Save to:** every place agentikeys found on your computer: the file the agent suggested, plus any
  secret store you're signed in to, with a project or vault to pick. Tick as many as you like.

If saving to one place fails, the page says which and stays open to retry. Places already saved are
marked and not written again.

## Using agents from another device

If you drive agents on one machine from another (SSH, T3 Code, a remote desktop), the page has to
reach you there:

| `--via` | Page is served on | Use when |
|---|---|---|
| `local` | `127.0.0.1`, opened in this machine's browser | you're at the machine |
| `tailscale` | this machine's Tailscale address only | **recommended** for other devices |
| `tunnel` | a temporary `*.trycloudflare.com` Cloudflare quick tunnel (needs `cloudflared`, no account) | anywhere, without Tailscale |
| `auto` (default) | `local`, or over SSH `tailscale` if it's running, else `tunnel` | |

**[Tailscale](https://tailscale.com/download) is the recommended way**: traffic stays between your
own devices, and nobody else serves the page. Set it once with `agentikeys config via tailscale` (or
`AGENTIKEYS_VIA=tailscale`); `--via` on a request overrides it. Tunnel links work anywhere but go
through Cloudflare, so tunnel pages suggest switching to Tailscale.

Real agentikeys links only ever point at `127.0.0.1`, your tailnet, or a `trycloudflare.com` tunnel
agentikeys started, and always show your security phrase.

## For agents

`agentikeys setup` installs these instructions; this is the detail behind them.

```sh
agentikeys request < spec.json          # prints {"status":"pending","id":…,"url":…} straight away
agentikeys wait <id> --timeout 60       # blocks until saved, cancelled or timed out; exit 4 = still pending
agentikeys request --foreground spec.json   # or block until the user is done
agentikeys log --key STRIPE_SECRET_KEY --json   # was this key set up before, and where?
```

The spec never contains values:

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

- `destination` is only a suggestion; the user decides on the page. `path` offers (and preticks) a
  file; `store` preticks a secret store (see [Secret stores](#secret-stores)). Both are optional.
- `destination.format` is `dotenv`, `json` or `toml`, inferred from the file name when omitted. For
  JSON and TOML, a field's `key` sets a dotted path (`"key": "database.password"`).
- `pattern` is a hint: a mismatch warns the user, who can still save.
- A field's `secret` sets its name in secret stores (defaults to `name`).

The receipt (one JSON line on stdout) never contains values:

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
| `pending` from `wait` | 4 |

`request` exits 0 with `pending` once the link is ready. A pending receipt may carry a `note` for
the user. `cancelled` and `timed_out` receipts still list `saved_to` if some places were saved first.

## Secret stores

The page offers every secret store whose CLI is installed; stores you're signed in to can be ticked,
the rest show how to sign in. Each uses its official CLI and the login you already have. Detection
only runs read-only commands, and values reach the CLI only on stdin, never as a command-line argument.

| `store.type` | CLI | Docs |
|---|---|---|
| `gcp_secret_manager` | `gcloud` | [docs/stores/gcp.md](docs/stores/gcp.md) |
| `aws_secrets_manager`, `aws_parameter_store` | `aws` | [docs/stores/aws.md](docs/stores/aws.md) |
| `azure_key_vault` | `az` | [docs/stores/azure.md](docs/stores/azure.md) |

An agent can pretick one, with options:
`"destination": {"store": {"type": "gcp_secret_manager", "project": "acme-prod"}}`. Before saving,
agentikeys re-checks every ticked store. Azure only allows letters, digits and hyphens, so it stores
`STRIPE_KEY` as `stripe-key` and the page says so. Each store's `saved_to` entry has `references`
(resource names, ARNs or URLs) an app can read the secret back by.

## Security

Files are written atomically with `0600` permissions and gitignored; agentikeys refuses files git
already tracks. Values are wiped from agentikeys' memory after saving. The local log
(`agentikeys log`) records which keys were requested and where they went, never values.

**Encryption.** In `local` and `tunnel` mode everything between your browser and agentikeys is
encrypted, on top of HTTPS. The link's `#` fragment, which browsers never send to any server, carries
two one-time keys:

- `k`, a P-256 public key: values are sealed to it (ECDH → HKDF-SHA256 → AES-256-GCM), so they stay
  private even from someone who has the link.
- `v`, a view key: the page's data (title, fields, places, your account and project names) and every
  reply are sealed with it (AES-256-GCM).

A network observer, including Cloudflare in tunnel mode, sees only the generic page code, the random
path, ciphertext and status codes. In `tailscale` mode the page isn't encrypted a second time:
browsers only allow WebCrypto on HTTPS or localhost, and WireGuard already encrypts the traffic end
to end between your devices.

**What it doesn't protect against:** an agent that is actively malicious, with shell access, can read
the files agentikeys writes. agentikeys keeps honest agents and their logs away from secrets; it isn't
a sandbox. Saving to a secret store rather than a local file leaves less on disk. See
[SECURITY.md](SECURITY.md) for the full model and how to report a vulnerability.

## Commands

| Command | Does |
|---|---|
| `agentikeys setup` | Teach the agents on this machine to use agentikeys |
| `agentikeys request` | Ask for values (agents) |
| `agentikeys wait <id>` | Collect a request's receipt (agents) |
| `agentikeys log` | Recent requests: keys, where saved, which project |
| `agentikeys phrase` | Show your security phrase (`--reset` for a new one) |
| `agentikeys config via <mode>` | Default way pages reach you |

## Platforms

macOS and Linux. Windows isn't supported yet.

## Roadmap

- `agentikeys run -- <cmd>`: hand secrets from stores or the OS keychain to a program at launch,
  so nothing sits in a readable file.
- An MCP server for harnesses that prefer tools over shell commands.
- More stores: 1Password, Vault/OpenBao, GitHub Actions secrets, Doppler, Infisical, and
  user-defined integrations.

## License

MIT
