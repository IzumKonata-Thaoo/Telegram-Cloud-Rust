# Security review of tgvault 0.1.0

Method: a full read-through of the source from an attacker's point of view, `cargo clippy`, `cargo audit` (RustSec), and exploit tests run against a live server (`tests/e2e.sh`, 55 checks). Every issue found has a regression test.

## Issues found and fixed

| # | Severity | Issue | How it could be exploited | Fix |
|---|---|---|---|---|
| 1 | High | Share-link download cap could be bypassed | Only requests starting at byte 0 were counted, so `Range: bytes=1-` downloaded without limit | Every GET without a download ticket is counted with one atomic `UPDATE`; a counted download issues an HMAC cookie so a player can seek for free |
| 2 | High | Unbounded memory during upload | Chunk tasks were spawned without limit and each held a chunk, so a fast client feeding a slow backend could exhaust RAM | The upload slot is acquired before the body is read further (TCP backpressure); memory is bounded by (slots + 1) chunks |
| 3 | High (behind a proxy) | `X-Forwarded-For` spoofing | The first entry, which the client controls, was trusted, bypassing IP allowlists and per-IP rate limits | Use the last hop, which the trusted proxy appended |
| 4 | Medium | Quota race | Parallel uploads all passed the pre-check and together exceeded the quota | Quota is checked inside the committing transaction |
| 5 | Medium | SQLite advisory (RUSTSEC-2026-0459, CVSS 6.8) | Outdated bundled SQLite | `rusqlite` upgraded |
| 6 | Low | `lru` unsoundness advisories | Potential use-after-free on a panic inside `pop()` | `lru` upgraded |
| 7 | Low | Hash-existence oracle | A key with only `write` scope could probe `link-by-hash` to learn whether a file exists | Requires `read` as well |
| 8 | Low | Authentication brute force and CPU exhaustion | No throttle on bad keys; unlimited concurrent Argon2 | 60 failures per minute per IP, 4-slot Argon2 semaphore, password length cap |
| 9 | Low | Possible panics in response building | `.unwrap()` on header construction | Mapped to controlled 500 errors |
| 10 | Low | Silent lockout on a malformed admin hash | An invalid `TGV_ADMIN_PASSWORD_HASH` disabled login without any error | Fail fast at startup |
| 11 | Medium | Bad-key throttle also blocked valid keys | Sending junk keys from one IP or NAT made real users receive 429 | A valid key is always honoured; only failed attempts are throttled |

## Defences verified by tests

- **Encryption at rest.** Searching stored chunks for a plaintext marker finds nothing. Tampered, swapped or cross-blob blocks fail authentication.
- **Authorization.** A key limited to one bucket cannot read, copy from, or presign in another. Object ids from other buckets return 404. Non-admin keys cannot create keys.
- **CSRF.** Cookie-authenticated state changes without `X-CSRF-Token` return 403.
- **Stored XSS.** User-uploaded HTML, SVG, XML, JavaScript and PDF are served as attachments with a sandbox CSP and `nosniff`. The panel never uses `innerHTML` with user data and has `script-src 'self'`.
- **Signed URLs.** Tampering, reuse across keys and wrong `Content-Type` on PUT links are rejected. Revoking a key kills its links.
- **Path handling.** `..` segments, control characters and backslashes in object keys are refused. Local backend locators are whitelisted.
- **SQL injection.** All queries use bound parameters. Prefix listing uses `substr` instead of `LIKE`.
- **Secrets.** API keys and share tokens are stored only as peppered HMACs and compared in constant time. Request errors strip URLs because Telegram URLs contain the bot token. Telegram `file_id`s are never returned by the API.

## Remote import and SSRF

`POST /buckets/{b}/import` makes the server fetch a user-supplied URL. Defences:

- http and https only, no credentials in the URL
- IP literals and every DNS answer must be globally routable. Blocked: loopback, private ranges, link-local (including cloud metadata), CGNAT, multicast, documentation and reserved ranges, and IPv4 hidden in IPv6 (mapped, NAT64, 6to4)
- filtering happens inside the resolver the connection really uses, so DNS rebinding cannot swap in a private address
- at most 5 redirects, each re-validated
- system proxies ignored, connect and read timeouts, size and quota limits enforced while streaming
- `Host` and hop-by-hop headers cannot be set

Tests confirm that `127.0.0.1`, `169.254.169.254`, `2130706433`, `[::ffff:127.0.0.1]`, `file://`, `localhost` and a hostname that resolves to `127.0.0.1` are all refused.

## Known limits

- The Telegram backend was verified end to end against a real channel during development, but the automated suite runs on the local backend (same `ChunkBackend` trait).
- Signed URLs issued from a panel session (`kid=admin`) cannot be revoked individually, only by expiry (7 days at most). Sign with an API key when you need revocation.
- Share links do not die with the key that created them. An admin can switch them off in the panel.
- A player that does not keep cookies counts each request as a download when a share has `max_downloads`.
- Panel sessions live in memory, so a restart signs everyone out. There is no upload slow-read timeout: put the server behind a reverse proxy with timeouts.
- Very verbose logging (`RUST_LOG=debug` for HTTP libraries) may print URLs that contain a bot token. The default logs only `tgvault=info`.
- Import jobs live in memory and are lost on restart.
- Telegram's availability, rate limits and terms are outside this project's control.
