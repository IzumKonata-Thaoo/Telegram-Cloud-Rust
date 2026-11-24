# tgvault Import API

A reference for building a download tool (or letting another AI build one) on top of tgvault's remote import.

Goal of the tool you are building: take files that live on another server or storage, and put them into a tgvault bucket (which stores them encrypted in Telegram) without the files passing through the machine that runs your tool. tgvault downloads the URL itself. Your tool only submits URLs, tracks jobs, retries, and reports.

Everything below was verified against a running server backed by a real Telegram channel. A tested reference client is `tools/tgv_import.py` (Python, standard library only). Machine-readable spec: `GET {BASE}/api/v1/openapi.json`. Human reference: `{BASE}/docs`.

## 1. Connection and auth

| Item | Value |
|---|---|
| Base URL | `TGV_BASE`, default `http://127.0.0.1:8787` |
| Auth | `Authorization: Bearer tgv_<12 chars>_<40 chars>` |
| Where the key comes from | environment variable `TGV_KEY`. Never hard-code or log it. |
| Content type | JSON bodies use `Content-Type: application/json` |
| Time | all timestamps are Unix seconds |

Scopes the key needs: `write` for import, `read` to download or verify, `list` to list. A key can also be limited to specific buckets, IPs and a requests-per-minute budget (default 600/min). If you get `403` the key lacks a scope or bucket; do not retry.

## 2. The import call

```
POST {BASE}/api/v1/buckets/{bucket}/import
```

Request body:

| Field | Type | Required | Notes |
|---|---|---|---|
| `url` | string | yes | Absolute `http://` or `https://` URL of the source file. |
| `key` | string | no | Object key in the vault. Default is the last path segment of the URL. Use `/` to make folders, for example `series/ep01.mp4`. |
| `content_type` | string | no | Defaults to the source's `Content-Type`, then to a guess from the key's extension. |
| `headers` | object | no | Extra headers sent to the source, for example `Referer`, `Authorization`, `Cookie`, `User-Agent`. At most 16. Forbidden names: `Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `Upgrade`, `TE`, `Trailer`, `Proxy-Authorization`. |
| `wait` | bool | no | `true` blocks until finished. Default `false`. |

Optional request header: `X-TGV-TTL: <seconds>` makes the stored object expire automatically.

Key rules (a violation is HTTP 400): 1 to 1024 bytes of UTF-8, no control characters, no backslash, no leading `/`, no `.` or `..` segments.

### Two ways to call it

**Asynchronous (recommended for anything big or for batches).** Without `wait`, the server answers `202 Accepted` with a job:

```json
{
  "id": "6f1c...",
  "bucket": "videos",
  "key": "series/ep01.mp4",
  "source": "https://media.example.com/ep01.mp4",
  "state": "queued",
  "received_bytes": 0,
  "total_bytes": null,
  "error": null,
  "object": null,
  "created_at": 1795446716,
  "finished_at": null
}
```

Poll `GET {BASE}/api/v1/imports/{id}` every 1 to 2 seconds. `state` goes `queued` then `downloading` then `storing` then `done` or `failed`.
- `received_bytes` grows while downloading; `total_bytes` is the source's `Content-Length` and is `null` if the source did not send one (then show bytes only, not a percentage).
- On `done`, `object` holds the stored object (see section 4).
- On `failed`, `error` holds a human-readable reason.
- `source` never includes the URL's query string, so presigned-URL signatures are not stored or shown. They are still used for the download.

**Synchronous.** With `"wait": true` the HTTP call stays open until the import ends. Success is `201` with the object. Failure is `400` with `{"error":{"code":"bad_request","message":"import failed: <reason>"}}`. Only use this for small files, or set a long client timeout (tens of minutes for big files), because proxies and clients commonly cut idle requests.

Other job endpoints:
- `GET {BASE}/api/v1/imports` lists the caller's recent jobs (admin keys see all).
- A job is visible only to the key that started it (others get 404).
- Jobs are kept in server memory for 24 hours after finishing.

## 3. What the server will and will not fetch

The server refuses to fetch anything that is not a public Internet address. This is an SSRF defence and is not configurable by the client. Refused with HTTP 400, message mentions "private or reserved" or "internal":

- schemes other than http/https (`file://`, `ftp://`, ...)
- `localhost`, `*.local`, `*.internal`, URLs with user:password@
- loopback, private (`10/8`, `172.16/12`, `192.168/16`), link-local (`169.254.0.0/16` incl. cloud metadata), CGNAT (`100.64/10`), multicast and reserved ranges, IPv6 equivalents, IPv4 hidden inside IPv6, numeric spellings such as `2130706433`
- hostnames whose DNS answer is only private addresses, and redirects that lead to any of the above (max 5 redirects, each re-checked)

Consequences for your design:
- If the source storage is on a private network, import cannot reach it. Either expose it on a public address with authentication, or have a machine on that network upload the bytes with `PUT {BASE}/api/v1/buckets/{bucket}/objects/{key}` (raw body).
- Do not try to bypass these checks. A 400 from them is final.

Other limits:
- Network timeouts: connect 15 s; the download fails if the source sends nothing for 60 s.
- Max file size: `TGV_MAX_UPLOAD_GIB` (default 64 GiB) and the bucket quota. Exceeding either fails the job (`413`-style message in `error`).
- The server runs at most **2 imports at the same time**. Extra jobs wait in `queued`. Submitting more than 2 in parallel is harmless but gives no speed-up.
- The server does not decrypt DRM, solve logins or CAPTCHAs, or follow JavaScript. It performs plain HTTP GETs. Supply any needed credentials through `headers`.

## 4. The stored object and how to verify it

A finished import returns:

```json
{
  "id": "685e...", "bucket": "videos", "key": "imports/bbb.mp4",
  "size": 991017, "content_type": "video/mp4",
  "etag": "77145c94c11f3754207499158df22406",
  "meta": {"source": "https://media.example.com/ep01.mp4"},
  "created_at": 1795446716, "expires_at": null, "deleted_at": null
}
```

- `size` is in bytes. Compare it with the source `Content-Length` if you know it.
- For imports and single uploads, **`etag` is the first 32 hex characters of the file's SHA-256**. If you know the SHA-256 of the source, check `sha256.startswith(etag)`. (Objects assembled by multipart upload have an etag of the form `<hex>-<parts>` that is not a hash.) This was confirmed by comparing against the hash of the downloaded source file.
- Re-importing to a key that already exists is not an error: the previous object moves to the trash (30 days by default) and the new one becomes current. Check first if you want idempotent behaviour (section 6).

## 5. Reading files back (for checks or downstream use)

| Need | Call |
|---|---|
| List a folder | `GET /api/v1/buckets/{bucket}/objects?prefix=series/&delimiter=/&limit=1000`, continue with `cursor=<next_cursor>` while `next_cursor` is not null |
| Does a key exist | `GET /api/v1/buckets/{bucket}/objects?prefix=<key>&limit=5` and look for an exact `key` match |
| Download | `GET /api/v1/buckets/{bucket}/objects/{key}`; percent-encode each path segment, keep `/` |
| Partial download or seek | same URL with `Range: bytes=0-1048575` (answers `206`) |
| Link for a browser or `<video>` tag | `POST /api/v1/presign` with `{"bucket","key","expires_in"}` returns a URL that needs no key |
| Delete | `DELETE .../objects/{key}` moves to trash; add `?permanent=true` to delete for good |

Do not expose `TGV_KEY` to a browser. Hand the browser a presigned URL instead.

## 6. Recommended tool behaviour

1. **Input**: accept a manifest (CSV, TSV or JSON) of `url`, `key`, optional `headers`. Reject invalid keys locally using the rules in section 2 before calling the API.
2. **Concurrency**: use 2 workers. Poll jobs rather than holding long HTTP calls open.
3. **Idempotency**: when re-running a manifest, skip keys that already exist (section 5) unless the user asks to overwrite.
4. **Retries**: retry only `429` (wait `Retry-After` seconds), `502`, `503`, and network errors, with exponential backoff (2 s doubling to 30 s, about 5 attempts). A `400`, `401`, `403`, `404` or `413` will not succeed on retry. Report it instead.
5. **Job failures**: a job with `state: "failed"` and a message such as "source answered HTTP 404/403" or "timed out" is about the source. A retry later may help for timeouts and 5xx, never for 404 or private-address errors.
6. **Server restarts lose running jobs.** If polling returns `404` for a job id you created, the server restarted. Resubmit that item (after the existence check, since it may have completed).
7. **Reporting**: print one line per item (`ok`, `skipped`, `FAILED`) with key, bytes, duration and the error. Exit non-zero if anything failed. Never print the API key or full presigned URLs.
8. **Verification**: if the manifest provides SHA-256 values, verify against `etag` (section 4).
9. **Speed expectation**: upload to Telegram through one bot is slow (about 10 MB in 50 s was measured on one connection). Large batches take a long time. More bots in the server's `TGV_BOT_TOKENS` raise throughput. That is a server setting, not something the tool can change.

## 7. Error format and status codes

Every error looks like:

```json
{"error": {"code": "bad_request", "message": "that address is private or reserved"}}
```

| Status | `code` | Meaning for your tool |
|---|---|---|
| 400 | `bad_request` | Invalid input (key, URL, headers) or the import failed at the source. Do not retry unchanged. |
| 401 | `unauthorized` | Missing, wrong, expired or revoked key. Stop. |
| 403 | `forbidden` | Key lacks a scope or bucket, or IP is not allowed. Stop. |
| 404 | `not_found` | Bucket or job does not exist. |
| 413 | `payload_too_large` | Over the size limit or bucket quota. |
| 429 | `rate_limited` | Wait the `Retry-After` header (seconds), then retry. |
| 502 | `backend_error` | Telegram side failed. Retry with backoff. |
| 500 | `internal` | Server fault. Retry a couple of times, then report. |

Repeated wrong keys from one IP get 429 (60 failures per minute). A valid key is never blocked by that.

## 8. Minimal examples

curl, one file, wait for it:

```bash
curl -sS -H "Authorization: Bearer $TGV_KEY" -H "Content-Type: application/json" \
  -d '{"url":"https://media.example.com/ep01.mp4","key":"series/ep01.mp4","wait":true}' \
  "$TGV_BASE/api/v1/buckets/videos/import"
```

Source that needs headers (for example a signed S3/R2 URL needs none; a private CDN may want a Referer):

```bash
curl -sS -H "Authorization: Bearer $TGV_KEY" -H "Content-Type: application/json" \
  -d '{"url":"https://cdn.example.com/p/ep02.mp4?token=abc","key":"series/ep02.mp4",
       "headers":{"Referer":"https://example.com/","User-Agent":"Mozilla/5.0"}}' \
  "$TGV_BASE/api/v1/buckets/videos/import"
```

Python with the bundled client (batch, 2 workers, skip files already stored):

```bash
export TGV_BASE=http://127.0.0.1:8787 TGV_KEY=...   # key from the panel's API keys page
python tools/tgv_import.py videos manifest.tsv --workers 2 --skip-existing
```

`manifest.tsv` has one item per line, tab separated: `<url><TAB><key>[<TAB>{"Referer":"..."}]`. Lines starting with `#` are comments. Example run on a real server: two valid URLs stored, a `http://127.0.0.1/...` URL rejected with "that address is private or reserved", and a 404 source reported as "source answered HTTP 404", exit code 1.

## 9. What you must not assume

- Do not assume a file is available until the job says `done` (or the synchronous call returned 201).
- Do not assume a job survives a server restart.
- Do not assume private-network sources work.
- Do not assume `total_bytes` is present.
- Do not store or print the API key, presigned URL query strings, or any bot token. The bot token and master keys are server secrets that your tool never needs.

## 10. Operational notes

About `TGV_KEY` and `TGV_BASE`:
- `TGV_KEY` is an API key issued by the server (panel -> API keys, or `POST /api/v1/keys` with an admin key). Its format is fixed: `tgv_` + 12 alphanumerics + `_` + 40 alphanumerics. You cannot choose its value, and a self-chosen password is rejected with 401. Only a hash is stored server-side, so a lost key cannot be recovered; create a new one and revoke the old one.
- `TGV_BASE` is the server's base URL, for example `http://127.0.0.1:8787`. It only works from the machine that runs the server. For other machines, expose the server over HTTPS and set `TGV_PUBLIC_URL`. Never send the key over plain HTTP across a network.
- Give each tool its own key with only what it needs: `write` for import, plus `read` and `list` for existence checks, limited to one bucket. Revoking a key also invalidates every signed URL it created.
- A sudden 401 or 403 after days of working usually means the key expired, was revoked, or the client IP changed.
- Environment variables set for your user account are only visible to terminals opened after they were set.

What a tool must never need: the Telegram bot token, `TGV_MASTER_KEYS`, `TGV_SERVER_SECRET` and the admin password. If a tool asks for them, the design is wrong.

Never put in logs, chat, code or commits: `TGV_KEY`, presigned URLs (their query string is a bearer credential), and `Cookie` or `Authorization` values passed in `headers`.

The server must be running. A restart loses in-memory import jobs, so resubmit anything that was still running.

