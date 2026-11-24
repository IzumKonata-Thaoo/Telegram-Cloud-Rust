# Security policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems.

Use GitHub's private reporting: open the repository's **Security** tab and choose **Report a vulnerability**. Include what you found, how to reproduce it, and the version or commit. Never include real bot tokens, master keys, API keys or channel ids in a report.

You can expect an acknowledgement within a few days. Fixes are released first, then described publicly.

## Supported versions

Only the latest commit on `main` is supported while the project is pre-1.0.

## Scope and design

The threat model and the findings of the internal review are in [docs/SECURITY-REVIEW.md](docs/SECURITY-REVIEW.md). In short:

- Telegram only receives ciphertext. Treat the Telegram channel as untrusted storage.
- The server holds the keys. Protect `TGV_MASTER_KEYS`, `TGV_SERVER_SECRET`, the bot tokens and the admin password like production secrets, and serve the panel over HTTPS.
- Run behind a reverse proxy with request timeouts, and set `TGV_TRUST_PROXY=true` only when that proxy appends `X-Forwarded-For`.
