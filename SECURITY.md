# Security

agentikeys handles secrets, so security reports are very welcome.

## Reporting a vulnerability

Please report privately through GitHub: **Security → Report a vulnerability** on this repository.
Don't open a public issue for a vulnerability. Expect a reply within a few days; once a fix is out,
you'll be credited in the release notes unless you'd rather not be.

## What agentikeys protects against

- Secrets ending up in AI chat transcripts, provider logs, synced chat history or screen shares:
  values go from the user's browser to agentikeys directly, and agents only get a receipt.
- Secrets being committed: files are written `0600`, gitignored, and agentikeys refuses files that
  git already tracks.
- Network observers, including Cloudflare in tunnel mode: page data, replies and values are
  encrypted end to end in the browser with keys carried in the link's `#` fragment (see the README).
- Lookalike pages hosted elsewhere: every real page shows the user's security phrase, which agents
  are never given.

## What it doesn't protect against

- **A malicious or fully compromised agent with shell access.** It can read files agentikeys
  writes, read the security phrase from agentikeys' config folder, or run its own lookalike page on
  the same machine. agentikeys keeps honest agents (and their logs) away from secrets; it isn't a
  sandbox. Saving to a cloud secret store instead of a local file narrows what's left on disk.
- **Cloudflare altering the page in tunnel mode.** Cloudflare serves the page code, so tunnel mode
  trusts it not to tamper with that code. Tailscale mode doesn't have this dependency.
- **Memory forensics.** Values are wiped from agentikeys' memory after saving, but copies made by
  the browser, the OS (pipe buffers) or a store's CLI are outside its control.
