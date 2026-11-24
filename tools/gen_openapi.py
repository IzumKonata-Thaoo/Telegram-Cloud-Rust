"""Generate web/openapi.json (OpenAPI 3.1) for tgvault. Run: python tools/gen_openapi.py"""
import json
import pathlib

def ref(name):
    return {"$ref": f"#/components/schemas/{name}"}

def jbody(schema, required=True, example=None):
    c = {"application/json": {"schema": schema}}
    if example is not None:
        c["application/json"]["example"] = example
    return {"required": required, "content": c}

def resp(desc, schema=None, headers=None):
    r = {"description": desc}
    if schema is not None:
        r["content"] = {"application/json": {"schema": schema}}
    if headers:
        r["headers"] = {k: {"schema": {"type": "string"}, "description": v} for k, v in headers.items()}
    return r

ERR = {
    "400": {"$ref": "#/components/responses/BadRequest"},
    "401": {"$ref": "#/components/responses/Unauthorized"},
    "403": {"$ref": "#/components/responses/Forbidden"},
    "404": {"$ref": "#/components/responses/NotFound"},
    "429": {"$ref": "#/components/responses/RateLimited"},
}

def errs(*codes):
    return {c: ERR[c] for c in codes}

def p(name, where="path", desc="", schema=None, required=None):
    return {"name": name, "in": where, "required": where == "path" if required is None else required,
            "description": desc, "schema": schema or {"type": "string"}}

BUCKET = p("bucket", desc="Bucket name")
KEY = p("key", desc="Object key; may contain `/`. Percent-encode each segment.")
NOAUTH = []

def op(tag, summary, desc, scope=None, params=None, body=None, responses=None, security=None, ex=None):
    o = {"tags": [tag], "summary": summary, "description": desc + (f"\n\n**Scope:** `{scope}`" if scope else ""),
         "responses": responses or {}}
    if params:
        o["parameters"] = params
    if body:
        o["requestBody"] = body
    if security is not None:
        o["security"] = security
    if ex:
        o["x-curl"] = ex
    return o

paths = {}
def add(path, method, o):
    paths.setdefault(path, {})[method] = o

# ---------------- auth ----------------
add("/api/v1/auth/login", "post", op("Panel session", "Sign in to the panel",
    "Exchanges the admin password for an HttpOnly, SameSite=Strict session cookie. Send the returned `csrf` value as `X-CSRF-Token` on every non-GET request made with the cookie. Limited to 10 attempts per minute per IP.",
    body=jbody({"type": "object", "required": ["password"], "properties": {"password": {"type": "string"}}}),
    responses={"200": resp("Signed in", {"type": "object", "properties": {"csrf": {"type": "string"}, "expires_at": {"type": "integer"}}}), **errs("401", "403", "429")},
    security=NOAUTH))
add("/api/v1/auth/logout", "post", op("Panel session", "Sign out", "Ends the panel session.", responses={"204": resp("Signed out")}))
add("/api/v1/auth/me", "get", op("Panel session", "Who am I", "Returns the caller's actor id, scopes and bucket restriction. Works with API keys too.",
    responses={"200": resp("Caller", {"type": "object"}), **errs("401")}, ex="curl -H \"Authorization: Bearer $TGV_KEY\" $BASE/api/v1/auth/me"))

# ---------------- buckets ----------------
add("/api/v1/buckets", "get", op("Buckets", "List buckets", "Buckets the caller may see, with object counts and bytes (live + trash).", scope="list",
    responses={"200": resp("Buckets", {"type": "object", "properties": {"buckets": {"type": "array", "items": ref("Bucket")}}}), **errs("401", "403")},
    ex="curl -H \"Authorization: Bearer $TGV_KEY\" $BASE/api/v1/buckets"))
add("/api/v1/buckets", "post", op("Buckets", "Create a bucket", "Bucket names are 3-63 characters of `a-z`, `0-9` and `-`.", scope="admin",
    body=jbody(ref("BucketInput"), example={"name": "media", "public": False, "cors_origins": ["https://example.com"], "quota_bytes": 107374182400}),
    responses={"201": resp("Created", ref("Bucket")), "409": {"$ref": "#/components/responses/Conflict"}, **errs("400", "401", "403")}))
add("/api/v1/buckets/{bucket}", "get", op("Buckets", "Get a bucket", "Settings and current usage.", scope="list", params=[BUCKET],
    responses={"200": resp("Bucket", ref("Bucket")), **errs("401", "403", "404")}))
add("/api/v1/buckets/{bucket}", "patch", op("Buckets", "Update bucket settings", "Send only the fields to change. `quota_bytes: 0` and `default_ttl: 0` remove the limit.", scope="admin", params=[BUCKET],
    body=jbody({"type": "object", "properties": {"public": {"type": "boolean"}, "cors_origins": {"type": "array", "items": {"type": "string"}}, "quota_bytes": {"type": "integer"}, "default_ttl": {"type": "integer"}}}),
    responses={"200": resp("Updated", ref("Bucket")), **errs("400", "401", "403", "404")}))
add("/api/v1/buckets/{bucket}", "delete", op("Buckets", "Delete a bucket", "Refuses while the bucket holds objects unless `force=true`, which deletes every object (including trash) and its chunks.", scope="admin",
    params=[BUCKET, p("force", "query", "Delete contents too", {"type": "boolean"})],
    responses={"204": resp("Deleted"), "409": {"$ref": "#/components/responses/Conflict"}, **errs("401", "403", "404")}))

# ---------------- objects ----------------
add("/api/v1/buckets/{bucket}/objects", "get", op("Objects", "List objects",
    "Lists live objects in key order. With `delimiter=/`, keys below the next `/` are folded into `common_prefixes` (folders). Pass `next_cursor` back as `cursor` for the next page.",
    scope="list", params=[BUCKET, p("prefix", "query", "Only keys starting with this"), p("delimiter", "query", "Usually `/`"), p("cursor", "query", "From `next_cursor`"),
                          p("limit", "query", "1-1000, default 200", {"type": "integer"})],
    responses={"200": resp("Page", {"type": "object", "properties": {"objects": {"type": "array", "items": ref("Object")}, "common_prefixes": {"type": "array", "items": {"type": "string"}}, "next_cursor": {"type": ["string", "null"]}}}), **errs("401", "403", "404")},
    ex="curl -H \"Authorization: Bearer $TGV_KEY\" \"$BASE/api/v1/buckets/media/objects?delimiter=/&prefix=videos/\""))
OBJ = "/api/v1/buckets/{bucket}/objects/{key}"
add(OBJ, "put", op("Objects", "Upload an object",
    "Streams the raw request body into encrypted chunks; nothing is buffered beyond one chunk per upload slot. An existing object at the key moves to the trash (previous version). "
    "Optional headers: `Content-Type`, `X-TGV-TTL` (seconds until automatic deletion), `X-TGV-Meta-<name>` (user metadata, 2 KB total), `If-None-Match: *` (fail if the key exists). "
    "Single uploads are limited by `TGV_MAX_UPLOAD_GIB`; use multipart for very large files or flaky links.",
    scope="write", params=[BUCKET, KEY, p("X-TGV-TTL", "header", "Seconds until the object expires", {"type": "integer"}, False), p("If-None-Match", "header", "`*` to refuse overwriting", None, False)],
    body={"required": True, "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}},
    responses={"201": resp("Stored", ref("Object")), "409": {"$ref": "#/components/responses/Conflict"}, "413": resp("Too large or over quota", ref("Error")), **errs("400", "401", "403", "404")},
    ex="curl -H \"Authorization: Bearer $TGV_KEY\" -H 'Content-Type: video/mp4' -T movie.mp4 $BASE/api/v1/buckets/media/objects/videos/movie.mp4"))
add(OBJ, "get", op("Objects", "Download or stream an object",
    "Returns the bytes, decrypting only the 64 KiB blocks a request touches. Supports `Range` (206 Partial Content, used by video players to seek), `If-None-Match` (304) and `If-Range`. "
    "`?download=1` forces a save dialog. HTML, SVG, XML and script types are always sent as attachments with a sandbox CSP.",
    scope="read", params=[BUCKET, KEY, p("Range", "header", "e.g. `bytes=0-1048575`", None, False), p("download", "query", "Force attachment", {"type": "boolean"})],
    responses={"200": {"description": "Whole object", "content": {"*/*": {"schema": {"type": "string", "format": "binary"}}}},
               "206": {"description": "Requested range", "headers": {"Content-Range": {"schema": {"type": "string"}}}},
               "304": resp("Not modified"), "416": resp("Range not satisfiable", ref("Error")), **errs("401", "403", "404")},
    ex="curl -H \"Authorization: Bearer $TGV_KEY\" -H 'Range: bytes=0-1048575' -o part.mp4 $BASE/api/v1/buckets/media/objects/videos/movie.mp4"))
add(OBJ, "head", op("Objects", "Object headers", "Same headers as GET without the body: size, type, ETag.", scope="read", params=[BUCKET, KEY], responses={"200": resp("Headers"), **errs("401", "403", "404")}))
add(OBJ, "delete", op("Objects", "Delete an object", "Moves the object to the trash. `permanent=true` deletes it at once; its chunks are removed from Telegram by the next clean-up when no other object shares them.",
    scope="delete", params=[BUCKET, KEY, p("permanent", "query", "Skip the trash", {"type": "boolean"})], responses={"204": resp("Deleted"), **errs("401", "403", "404")}))
add("/api/v1/buckets/{bucket}/copy", "post", op("Objects", "Copy an object", "Server-side copy that shares the encrypted chunks (no re-upload). Needs `read` on the source bucket and `write` on this one.",
    scope="read + write", params=[BUCKET], body=jbody({"type": "object", "required": ["source_key", "dest_key"], "properties": {"source_key": {"type": "string"}, "source_bucket": {"type": "string"}, "dest_key": {"type": "string"}}}),
    responses={"201": resp("Copied", ref("Object")), **errs("400", "401", "403", "404")}))
add("/api/v1/buckets/{bucket}/link-by-hash", "post", op("Objects", "Instant upload by SHA-256",
    "If this bucket already stores bytes with the given SHA-256, creates `key` pointing at them without sending the data. Returns 404 when there is no match, in which case upload normally. Matching is limited to the same bucket so a hash alone cannot unlock someone else's file.",
    scope="write", params=[BUCKET], body=jbody({"type": "object", "required": ["key", "sha256"], "properties": {"key": {"type": "string"}, "sha256": {"type": "string"}, "content_type": {"type": "string"}}}),
    responses={"201": resp("Linked", ref("Object")), **errs("400", "401", "403", "404")}))
add("/api/v1/buckets/{bucket}/trash", "get", op("Trash", "List the trash", "Deleted and overwritten objects, newest first. They are purged after `TGV_TRASH_DAYS`.", scope="list", params=[BUCKET],
    responses={"200": resp("Trash", {"type": "object", "properties": {"objects": {"type": "array", "items": ref("Object")}, "purge_after_days": {"type": "integer"}}}), **errs("401", "403", "404")}))
add("/api/v1/buckets/{bucket}/trash", "delete", op("Trash", "Empty the trash", "Deletes every trashed object in the bucket for good.", scope="delete", params=[BUCKET], responses={"200": resp("Count", {"type": "object", "properties": {"deleted": {"type": "integer"}}}), **errs("401", "403", "404")}))
add("/api/v1/objects/{id}", "get", op("Objects", "Object details by id", "Metadata plus the chunk map: each chunk's offset, size and the bot holding it (never Telegram file ids).", scope="read",
    params=[p("id", desc="Object id")], responses={"200": resp("Object with chunks", ref("Object")), **errs("401", "403", "404")}))
add("/api/v1/objects/{id}/restore", "post", op("Trash", "Restore from the trash", "Fails with 409 if a live object already uses the key.", scope="write", params=[p("id", desc="Object id")],
    responses={"204": resp("Restored"), "409": {"$ref": "#/components/responses/Conflict"}, **errs("401", "403", "404")}))

# ---------------- remote import ----------------
add("/api/v1/buckets/{bucket}/import", "post", op("Remote import", "Import a file from a URL",
    "The server downloads `url` itself and streams it, encrypted, straight into the bucket: nothing is written to disk and your client does not relay the bytes. "
    "Returns 202 with a job to poll, or with `wait: true` blocks and returns the object (201). "
    "Only public http/https addresses are fetched: loopback, private, link-local (cloud metadata), CGNAT and other reserved ranges are refused, including after DNS resolution and on every redirect (max 5). "
    "`headers` are sent to the source (for example `Referer` or `Authorization`); `Host` and hop-by-hop headers are not allowed. Size limits and bucket quota apply while streaming.",
    scope="write", params=[BUCKET, p("X-TGV-TTL", "header", "Seconds until the imported object expires", {"type": "integer"}, False)],
    body=jbody({"type": "object", "required": ["url"], "properties": {"url": {"type": "string"}, "key": {"type": "string", "description": "Defaults to the last URL path segment"},
                "content_type": {"type": "string"}, "headers": {"type": "object", "additionalProperties": {"type": "string"}}, "wait": {"type": "boolean"}}},
               example={"url": "https://media.example.com/episodes/ep01.mp4", "key": "series/ep01.mp4", "headers": {"Referer": "https://media.example.com/"}}),
    responses={"202": resp("Job started", ref("ImportJob")), "201": resp("Imported (wait=true)", ref("Object")), **errs("400", "401", "403", "404")},
    ex="curl -H \"Authorization: Bearer $TGV_KEY\" -H 'Content-Type: application/json' -d '{\"url\":\"https://media.example.com/ep01.mp4\",\"key\":\"series/ep01.mp4\",\"wait\":true}' $BASE/api/v1/buckets/media/import"))
add("/api/v1/imports/{id}", "get", op("Remote import", "Import progress", "State is `queued`, `downloading`, `storing`, `done` or `failed`. Jobs are kept for 24 hours after they finish.",
    scope="write", params=[p("id", desc="Job id")], responses={"200": resp("Job", ref("ImportJob")), **errs("401", "404")}))
add("/api/v1/imports", "get", op("Remote import", "List recent imports", "Imports started by the caller (admins see all).", scope="write", responses={"200": resp("Jobs", {"type": "object"}), **errs("401", "403")}))

# ---------------- multipart ----------------
add("/api/v1/buckets/{bucket}/uploads", "post", op("Multipart uploads", "Start a multipart upload",
    "Returns an `upload_id`. Then PUT parts numbered from 1, in any order and in parallel; re-sending a part replaces it. Uploads not completed within 24 hours are aborted.",
    scope="write", params=[BUCKET], body=jbody({"type": "object", "required": ["key"], "properties": {"key": {"type": "string"}, "content_type": {"type": "string"}}}),
    responses={"201": resp("Started", {"type": "object", "properties": {"upload_id": {"type": "string"}, "chunk_size": {"type": "integer"}, "max_parts": {"type": "integer"}, "expires_at": {"type": "integer"}}}), **errs("400", "401", "403", "404")},
    ex="curl -H \"Authorization: Bearer $TGV_KEY\" -H 'Content-Type: application/json' -d '{\"key\":\"big.iso\"}' $BASE/api/v1/buckets/media/uploads"))
add("/api/v1/buckets/{bucket}/uploads", "get", op("Multipart uploads", "List open uploads", "Uploads started by the caller (admins see all).", scope="write", params=[BUCKET], responses={"200": resp("Uploads", {"type": "object"}), **errs("401", "403", "404")}))
add("/api/v1/uploads/{id}", "get", op("Multipart uploads", "Upload progress", "Parts received so far, so an interrupted client knows what to resend.", scope="write", params=[p("id", desc="Upload id")], responses={"200": resp("Status", {"type": "object"}), **errs("401", "404")}))
add("/api/v1/uploads/{id}", "delete", op("Multipart uploads", "Abort an upload", "Discards received parts.", scope="write", params=[p("id", desc="Upload id")], responses={"204": resp("Aborted"), **errs("401", "404")}))
add("/api/v1/uploads/{id}/parts/{n}", "put", op("Multipart uploads", "Upload a part", "Raw body. Parts can be any size; each is split into chunks server-side.", scope="write",
    params=[p("id", desc="Upload id"), p("n", desc="Part number 1-10000", schema={"type": "integer"})], body={"required": True, "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}},
    responses={"200": resp("Part stored", {"type": "object", "properties": {"part": {"type": "integer"}, "size": {"type": "integer"}, "sha256": {"type": "string"}}}), **errs("400", "401", "404")},
    ex="curl -H \"Authorization: Bearer $TGV_KEY\" -T part1.bin $BASE/api/v1/uploads/$UPLOAD_ID/parts/1"))
add("/api/v1/uploads/{id}/complete", "post", op("Multipart uploads", "Complete an upload", "Joins parts 1..N in order. Fails if a part number is missing. Accepts the same `X-TGV-TTL` and `X-TGV-Meta-*` headers as a single PUT.",
    scope="write", params=[p("id", desc="Upload id")], responses={"201": resp("Object created", ref("Object")), **errs("400", "401", "404")}))

# ---------------- presign & public ----------------
add("/api/v1/presign", "post", op("Signed URLs", "Create a signed URL",
    "Returns a URL anyone can use until it expires (max 7 days), like an R2 presigned URL. GET links stream with Range support, so they work as `<video src>` on any site listed in the bucket's CORS origins. PUT links let a browser upload directly; set `content_type` to pin the upload's type. Revoking or down-scoping the signing key invalidates its links.",
    scope="read (GET) / write (PUT)", body=jbody(ref("PresignInput"), example={"bucket": "media", "key": "videos/movie.mp4", "method": "GET", "expires_in": 3600}),
    responses={"200": resp("URL", {"type": "object", "properties": {"url": {"type": "string"}, "method": {"type": "string"}, "expires_at": {"type": "integer"}}}), **errs("400", "401", "403", "404")},
    ex="curl -H \"Authorization: Bearer $TGV_KEY\" -H 'Content-Type: application/json' -d '{\"bucket\":\"media\",\"key\":\"videos/movie.mp4\",\"expires_in\":3600}' $BASE/api/v1/presign"))
add("/p/{bucket}/{key}", "get", op("Signed URLs", "Use a signed GET URL", "No credentials: the query string carries `m`, `exp`, `kid`, `sig`. Range requests and CORS are supported.", params=[BUCKET, KEY],
    responses={"200": resp("Bytes"), "206": resp("Range"), "403": resp("Invalid or expired link", ref("Error"))}, security=NOAUTH))
add("/p/{bucket}/{key}", "put", op("Signed URLs", "Use a signed PUT URL", "Upload the raw body. Must send the Content-Type the link was signed for, if any.", params=[BUCKET, KEY],
    responses={"201": resp("Stored", ref("Object")), "403": resp("Invalid link or wrong Content-Type", ref("Error"))}, security=NOAUTH))
add("/pub/{bucket}/{key}", "get", op("Public buckets", "Read from a public bucket", "Like an r2.dev URL: no credentials, cacheable for 5 minutes, Range supported. Private buckets answer 404.", params=[BUCKET, KEY],
    responses={"200": resp("Bytes"), "206": resp("Range"), "404": resp("Missing or private", ref("Error"))}, security=NOAUTH,
    ex="<video src=\"$BASE/pub/media/videos/trailer.mp4\" controls></video>"))

# ---------------- shares ----------------
add("/api/v1/shares", "post", op("Share links", "Create a share link",
    "A link to one object with an optional password (argon2-hashed), expiry and download cap. The landing page at `url` plays video, audio and images. Each counted download issues a 6-hour cookie so seeking does not use up the cap.",
    scope="share + read", body=jbody(ref("ShareInput"), example={"bucket": "media", "key": "videos/movie.mp4", "password": "for-alex", "expires_in": 604800, "max_downloads": 5}),
    responses={"201": resp("Created", {"type": "object", "properties": {"id": {"type": "string"}, "url": {"type": "string"}, "raw_url": {"type": "string"}}}), **errs("400", "401", "403", "404")}))
add("/api/v1/shares", "get", op("Share links", "List share links", "Links the caller created (admins see all).", scope="share", responses={"200": resp("Links", {"type": "object"}), **errs("401", "403")}))
add("/api/v1/shares/{id}", "delete", op("Share links", "Turn off a share link", "The link answers 404 from now on.", scope="share", params=[p("id", desc="Share id")], responses={"204": resp("Turned off"), **errs("401", "403", "404")}))
add("/s/{token}", "get", op("Share links", "Share landing page", "HTML page with a player or password form.", params=[p("token", desc="From the share URL")], responses={"200": {"description": "HTML"}, "404": {"description": "Unavailable"}}, security=NOAUTH))
add("/s/{token}/raw", "get", op("Share links", "Share bytes", "Raw bytes with Range support. Password-protected shares need the unlock cookie.", params=[p("token", desc="From the share URL"), p("download", "query", "Force attachment", {"type": "boolean"})],
    responses={"200": resp("Bytes"), "206": resp("Range"), "403": resp("Locked", ref("Error")), "404": resp("Unavailable", ref("Error"))}, security=NOAUTH))

# ---------------- keys ----------------
add("/api/v1/keys", "post", op("API keys", "Create an API key",
    "Returns the key once; only an HMAC of the secret is stored. Restrict it by scope, bucket, IP/CIDR, rate and lifetime, like an R2 API token.",
    scope="admin", body=jbody(ref("KeyInput"), example={"name": "website", "scopes": ["read", "list"], "buckets": ["media"], "allowed_ips": ["203.0.113.0/24"], "rpm": 1200, "expires_in": 7776000}),
    responses={"201": resp("Created", {"type": "object", "properties": {"id": {"type": "string"}, "key": {"type": "string"}}}), **errs("400", "401", "403")}))
add("/api/v1/keys", "get", op("API keys", "List API keys", "Never returns secrets.", scope="admin", responses={"200": resp("Keys", {"type": "object"}), **errs("401", "403")}))
add("/api/v1/keys/{id}", "delete", op("API keys", "Revoke an API key", "Takes effect immediately, including for signed URLs it issued.", scope="admin", params=[p("id", desc="Key id (the part after tgv_)")], responses={"204": resp("Revoked"), **errs("401", "403", "404")}))

# ---------------- admin ----------------
add("/api/v1/admin/stats", "get", op("Admin", "Storage and health stats", "Totals, per-bucket usage, cache hit rate, bot health and cool-downs.", scope="admin", responses={"200": resp("Stats", {"type": "object"}), **errs("401", "403")}))
add("/api/v1/admin/audit", "get", op("Admin", "Audit log", "Newest first. Page with `before=<id>`.", scope="admin",
    params=[p("limit", "query", "1-1000", {"type": "integer"}), p("before", "query", "Event id", {"type": "integer"}), p("actor", "query", "Filter by actor")], responses={"200": resp("Events", {"type": "object"}), **errs("401", "403")}))
add("/api/v1/admin/backups", "get", op("Admin", "List metadata backups", "", scope="admin", responses={"200": resp("Backups", {"type": "object"}), **errs("401", "403")}))
add("/api/v1/admin/backups", "post", op("Admin", "Back up metadata now", "Seals a SQLite snapshot with the active master key, stores it in the backend and pins it in the Telegram channel.", scope="admin", responses={"200": resp("Backup", {"type": "object"}), **errs("401", "403")}))
add("/api/v1/admin/gc", "post", op("Admin", "Run clean-up now", "Expires objects past their TTL, purges old trash, aborts stale uploads and deletes unreferenced chunks from Telegram.", scope="admin", responses={"200": resp("Report", {"type": "object"}), **errs("401", "403")}))
add("/api/v1/admin/rewrap", "post", op("Admin", "Re-wrap data keys", "After adding a new master key version and making it active, re-encrypts every per-file data key with it. Chunks are not touched.", scope="admin", responses={"200": resp("Report", {"type": "object"}), **errs("401", "403")}))
add("/healthz", "get", op("Admin", "Health check", "For load balancers and uptime checks.", responses={"200": resp("Health", {"type": "object"})}, security=NOAUTH))

def err_resp(desc):
    return {"description": desc, "content": {"application/json": {"schema": ref("Error")}}}

spec = {
    "openapi": "3.1.0",
    "info": {
        "title": "tgvault API",
        "version": "0.1.0",
        "description": "Encrypted object storage on Telegram. Authenticate with `Authorization: Bearer tgv_<id>_<secret>`. Every request is rate limited per key; public routes are limited per IP. Errors share one shape: `{\"error\": {\"code\", \"message\"}}`.",
    },
    "servers": [{"url": "http://127.0.0.1:8787"}],
    "security": [{"bearer": []}],
    "tags": [{"name": n} for n in ["Buckets", "Objects", "Remote import", "Trash", "Multipart uploads", "Signed URLs", "Public buckets", "Share links", "API keys", "Admin", "Panel session"]],
    "paths": paths,
    "components": {
        "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer", "description": "API key created in the panel or via POST /api/v1/keys"}},
        "responses": {
            "BadRequest": err_resp("The request is malformed; the message says which field."),
            "Unauthorized": err_resp("No valid key or session."),
            "Forbidden": err_resp("The key lacks a scope, bucket or IP permission, or the CSRF token is missing."),
            "NotFound": err_resp("Not found, or not visible to this key."),
            "Conflict": err_resp("The resource already exists or is in use."),
            "RateLimited": {"description": "Too many requests; wait `Retry-After` seconds.", "headers": {"Retry-After": {"schema": {"type": "integer"}}}, "content": {"application/json": {"schema": ref("Error")}}},
        },
        "schemas": {
            "Error": {"type": "object", "properties": {"error": {"type": "object", "properties": {"code": {"type": "string"}, "message": {"type": "string"}}}}},
            "Bucket": {"type": "object", "properties": {"id": {"type": "string"}, "name": {"type": "string"}, "public": {"type": "boolean"}, "cors_origins": {"type": "array", "items": {"type": "string"}},
                                                        "quota_bytes": {"type": ["integer", "null"]}, "default_ttl": {"type": ["integer", "null"]}, "created_at": {"type": "integer"}, "objects": {"type": "integer"}, "bytes": {"type": "integer"}}},
            "BucketInput": {"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}, "public": {"type": "boolean"}, "cors_origins": {"type": "array", "items": {"type": "string"}, "description": "`https://site.example` or `*`"},
                                                                                "quota_bytes": {"type": "integer"}, "default_ttl": {"type": "integer", "description": "Seconds; new objects expire after this"}}},
            "Object": {"type": "object", "properties": {"id": {"type": "string"}, "bucket": {"type": "string"}, "key": {"type": "string"}, "size": {"type": "integer"}, "content_type": {"type": "string"}, "etag": {"type": "string"},
                                                        "meta": {"type": "object"}, "created_at": {"type": "integer"}, "expires_at": {"type": ["integer", "null"]}, "deleted_at": {"type": ["integer", "null"]}}},
            "ImportJob": {"type": "object", "properties": {"id": {"type": "string"}, "bucket": {"type": "string"}, "key": {"type": "string"}, "source": {"type": "string", "description": "URL without query string"},
                                                           "state": {"type": "string", "enum": ["queued", "downloading", "storing", "done", "failed"]}, "received_bytes": {"type": "integer"}, "total_bytes": {"type": ["integer", "null"]},
                                                           "error": {"type": ["string", "null"]}, "object": {"oneOf": [ref("Object"), {"type": "null"}]}, "created_at": {"type": "integer"}, "finished_at": {"type": ["integer", "null"]}}},
            "PresignInput": {"type": "object", "required": ["bucket", "key"], "properties": {"bucket": {"type": "string"}, "key": {"type": "string"}, "method": {"type": "string", "enum": ["GET", "PUT"]}, "expires_in": {"type": "integer", "description": "1-604800 seconds, default 3600"}, "content_type": {"type": "string"}}},
            "ShareInput": {"type": "object", "required": ["bucket", "key"], "properties": {"bucket": {"type": "string"}, "key": {"type": "string"}, "password": {"type": "string"}, "expires_in": {"type": "integer"}, "max_downloads": {"type": "integer"}}},
            "KeyInput": {"type": "object", "required": ["name", "scopes"], "properties": {"name": {"type": "string"}, "scopes": {"type": "array", "items": {"type": "string", "enum": ["read", "write", "delete", "list", "share", "admin"]}},
                                                                                     "buckets": {"type": "array", "items": {"type": "string"}}, "allowed_ips": {"type": "array", "items": {"type": "string"}}, "rpm": {"type": "integer"}, "expires_in": {"type": "integer"}}},
        },
    },
}

out = pathlib.Path(__file__).resolve().parent.parent / "web" / "openapi.json"
out.write_text(json.dumps(spec, indent=1, ensure_ascii=False), encoding="utf-8")
print(f"wrote {out} ({len(paths)} paths, {sum(len(v) for v in paths.values())} operations)")
