//! SQLite metadata store. The bytes live in the chunk backend; this file is the map.

use crate::error::{ApiError, ApiResult};
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Arc;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS buckets (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    public        INTEGER NOT NULL DEFAULT 0,
    cors_origins  TEXT NOT NULL DEFAULT '[]',
    quota_bytes   INTEGER,
    default_ttl   INTEGER,
    created_at    INTEGER NOT NULL
);

-- A blob is one encrypted byte stream. Objects point at blobs, so copies and
-- hash-matched uploads share chunks (refcount) instead of re-uploading.
CREATE TABLE IF NOT EXISTS blobs (
    id            TEXT PRIMARY KEY,
    size          INTEGER NOT NULL DEFAULT 0,
    sha256        TEXT,
    wrapped_dek   BLOB NOT NULL,
    kek_version   INTEGER NOT NULL,
    state         TEXT NOT NULL,            -- uploading | ready | orphan
    refcount      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS blobs_gc ON blobs(state, refcount);
CREATE INDEX IF NOT EXISTS blobs_sha ON blobs(sha256);

CREATE TABLE IF NOT EXISTS chunks (
    blob_id    TEXT NOT NULL REFERENCES blobs(id) ON DELETE CASCADE,
    part       INTEGER NOT NULL,            -- multipart part number (0 for single PUT)
    seq        INTEGER NOT NULL,            -- order within the part
    ord        INTEGER,                     -- global order, set when the blob is sealed
    offset     INTEGER,                     -- plaintext offset in the blob
    plain_len  INTEGER NOT NULL,
    salt       BLOB NOT NULL,
    backend    TEXT NOT NULL,
    locator    TEXT NOT NULL,
    PRIMARY KEY (blob_id, part, seq)
);
CREATE INDEX IF NOT EXISTS chunks_ord ON chunks(blob_id, ord);

-- Chunks whose backend copy still has to be deleted (GC queue).
CREATE TABLE IF NOT EXISTS dead_chunks (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    backend   TEXT NOT NULL,
    locator   TEXT NOT NULL,
    attempts  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS objects (
    id            TEXT PRIMARY KEY,
    bucket_id     TEXT NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    key           TEXT NOT NULL,
    blob_id       TEXT NOT NULL REFERENCES blobs(id),
    size          INTEGER NOT NULL,
    content_type  TEXT NOT NULL,
    etag          TEXT NOT NULL,
    meta          TEXT NOT NULL DEFAULT '{}',
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER,
    deleted_at    INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS objects_live_key ON objects(bucket_id, key) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS objects_trash ON objects(bucket_id, deleted_at);
CREATE INDEX IF NOT EXISTS objects_expiry ON objects(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS uploads (
    id            TEXT PRIMARY KEY,
    bucket_id     TEXT NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    key           TEXT NOT NULL,
    blob_id       TEXT NOT NULL REFERENCES blobs(id),
    content_type  TEXT NOT NULL,
    owner         TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS api_keys (
    id            TEXT PRIMARY KEY,         -- public part: tgv_<id>_<secret>
    name          TEXT NOT NULL,
    secret_hash   TEXT NOT NULL,
    scopes        TEXT NOT NULL,            -- JSON array
    buckets       TEXT,                     -- JSON array of bucket names, NULL = all
    allowed_ips   TEXT,                     -- JSON array of IPs/CIDRs, NULL = any
    rpm           INTEGER,
    expires_at    INTEGER,
    revoked_at    INTEGER,
    last_used_at  INTEGER,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS shares (
    id             TEXT PRIMARY KEY,
    token_hash     TEXT NOT NULL UNIQUE,
    object_id      TEXT NOT NULL REFERENCES objects(id) ON DELETE CASCADE,
    password_hash  TEXT,
    expires_at     INTEGER,
    max_downloads  INTEGER,
    downloads      INTEGER NOT NULL DEFAULT 0,
    created_by     TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    revoked_at     INTEGER
);

CREATE TABLE IF NOT EXISTS audit (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts       INTEGER NOT NULL,
    actor    TEXT NOT NULL,
    action   TEXT NOT NULL,
    target   TEXT NOT NULL,
    ip       TEXT NOT NULL,
    detail   TEXT
);
CREATE INDEX IF NOT EXISTS audit_ts ON audit(ts);

CREATE TABLE IF NOT EXISTS backups (
    id          TEXT PRIMARY KEY,
    created_at  INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    backend     TEXT NOT NULL,
    locator     TEXT NOT NULL
);
"#;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Run a closure against the connection on the blocking pool.
    pub async fn call<F, R>(&self, f: F) -> ApiResult<R>
    where
        F: FnOnce(&mut Connection) -> ApiResult<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock();
            f(&mut guard)
        })
        .await
        .map_err(|e| ApiError::Internal(format!("db task: {e}")))?
    }

    pub async fn audit(&self, actor: &str, action: &str, target: &str, ip: &str, detail: Option<String>) {
        let (actor, action, target, ip) = (actor.to_string(), action.to_string(), target.to_string(), ip.to_string());
        let r = self
            .call(move |c| {
                c.execute(
                    "INSERT INTO audit (ts, actor, action, target, ip, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![now(), actor, action, target, ip, detail],
                )?;
                Ok(())
            })
            .await;
        if let Err(e) = r {
            tracing::warn!("audit write failed: {e}");
        }
    }
}
