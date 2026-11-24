//! Object engine: streams uploads into encrypted chunks, maps byte ranges back
//! onto chunks for reads, and keeps blob reference counts for copies/dedup.

use crate::cache::ChunkCache;
use crate::crypto::{random_bytes, ChunkCipher, Keyring};
use crate::db::{now, Db};
use crate::error::{ApiError, ApiResult};
use bytes::{Bytes, BytesMut};
use futures::{Stream, StreamExt};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[derive(Debug, Clone, Serialize)]
pub struct Bucket {
    pub id: String,
    pub name: String,
    pub public: bool,
    pub cors_origins: Vec<String>,
    pub quota_bytes: Option<i64>,
    pub default_ttl: Option<i64>,
    pub created_at: i64,
}

impl Bucket {
    pub const COLS: &'static str = "id, name, public, cors_origins, quota_bytes, default_ttl, created_at";
    pub fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Self> {
        let cors: String = r.get(3)?;
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            public: r.get::<_, i64>(2)? != 0,
            cors_origins: serde_json::from_str(&cors).unwrap_or_default(),
            quota_bytes: r.get(4)?,
            default_ttl: r.get(5)?,
            created_at: r.get(6)?,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Object {
    pub id: String,
    pub bucket: String,
    pub key: String,
    pub size: i64,
    pub content_type: String,
    pub etag: String,
    pub meta: Value,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub deleted_at: Option<i64>,
    #[serde(skip)]
    pub blob_id: String,
    #[serde(skip)]
    pub bucket_id: String,
}

impl Object {
    pub const COLS: &'static str = "o.id, b.name, o.key, o.size, o.content_type, o.etag, o.meta, o.created_at, o.expires_at, o.deleted_at, o.blob_id, o.bucket_id";
    pub fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Self> {
        let meta: String = r.get(6)?;
        Ok(Self {
            id: r.get(0)?,
            bucket: r.get(1)?,
            key: r.get(2)?,
            size: r.get(3)?,
            content_type: r.get(4)?,
            etag: r.get(5)?,
            meta: serde_json::from_str(&meta).unwrap_or(Value::Null),
            created_at: r.get(7)?,
            expires_at: r.get(8)?,
            deleted_at: r.get(9)?,
            blob_id: r.get(10)?,
            bucket_id: r.get(11)?,
        })
    }
}

#[derive(Debug, Clone)]
struct ChunkRow {
    offset: u64,
    plain_len: u64,
    salt: [u8; 8],
    locator: String,
}

pub struct NewObject {
    pub bucket_id: String,
    pub key: String,
    pub content_type: String,
    pub meta: Value,
    pub expires_at: Option<i64>,
}

/// Quota check inside the committing transaction. Upload-time checks are only a
/// fast path; this is what makes the quota hold under concurrent uploads.
fn check_quota(tx: &rusqlite::Transaction, bucket_id: &str, adding: i64) -> ApiResult<()> {
    let (quota, used): (Option<i64>, i64) = tx.query_row(
        "SELECT quota_bytes, (SELECT COALESCE(SUM(size), 0) FROM objects WHERE bucket_id = ?1) FROM buckets WHERE id = ?1",
        [bucket_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    match quota {
        Some(q) if used + adding > q => Err(ApiError::PayloadTooLarge(format!("bucket quota exceeded: {} bytes left", (q - used).max(0)))),
        _ => Ok(()),
    }
}

pub struct Engine {
    pub db: Db,
    pub cache: Arc<ChunkCache>,
    pub keys: Arc<Keyring>,
    pub chunk_size: usize,
    pub max_upload: u64,
    upload_slots: Arc<Semaphore>,
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

impl Engine {
    pub fn new(db: Db, cache: Arc<ChunkCache>, keys: Arc<Keyring>, chunk_size: usize, max_upload: u64, concurrency: usize) -> Self {
        Self { db, cache, keys, chunk_size, max_upload, upload_slots: Arc::new(Semaphore::new(concurrency.max(1))) }
    }

    async fn slot(&self) -> ApiResult<tokio::sync::OwnedSemaphorePermit> {
        self.upload_slots.clone().acquire_owned().await.map_err(|_| ApiError::Internal("upload pool closed".into()))
    }

    pub async fn create_blob(&self) -> ApiResult<(String, [u8; 32])> {
        let id = new_id();
        let dek = Keyring::new_dek();
        let (wrapped, ver) = self.keys.wrap_dek(&dek, &id)?;
        let bid = id.clone();
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT INTO blobs (id, wrapped_dek, kek_version, state, refcount, created_at) VALUES (?1, ?2, ?3, 'uploading', 0, ?4)",
                    params![bid, wrapped, ver, now()],
                )?;
                Ok(())
            })
            .await?;
        Ok((id, dek))
    }

    pub async fn blob_dek(&self, blob_id: &str) -> ApiResult<[u8; 32]> {
        let bid = blob_id.to_string();
        let (wrapped, ver): (Vec<u8>, u32) = self
            .db
            .call(move |c| Ok(c.query_row("SELECT wrapped_dek, kek_version FROM blobs WHERE id = ?1", [bid], |r| Ok((r.get(0)?, r.get(1)?)))?))
            .await?;
        Ok(self.keys.unwrap_dek(&wrapped, ver, blob_id)?)
    }

    /// Stream `body` into sealed chunks for `part` of a blob. Returns (bytes, sha256 hex).
    /// Uploads run concurrently (bounded); every uploaded chunk is recorded even on
    /// failure, so the garbage collector can delete it from the backend later.
    pub async fn write_part<S, E>(&self, blob_id: &str, dek: [u8; 32], part: i64, mut body: S, limit: u64) -> ApiResult<(u64, String)>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let cipher = Arc::new(ChunkCipher::new(&dek, blob_id));
        let mut hasher = Sha256::new();
        let mut buf = BytesMut::with_capacity(self.chunk_size);
        let mut total: u64 = 0;
        let mut seq: i64 = 0;
        let mut tasks: JoinSet<ApiResult<()>> = JoinSet::new();
        let mut failure: Option<ApiError> = None;

        // The upload slot is taken *before* the task is spawned. While all slots are
        // busy the request body is not read any further (TCP backpressure), so memory
        // stays bounded by (slots + 1) chunks per server instead of growing with
        // how fast a client can push bytes at a slow backend.
        let flush = |data: Bytes, seq: i64, permit: tokio::sync::OwnedSemaphorePermit, tasks: &mut JoinSet<ApiResult<()>>| {
            let cipher = cipher.clone();
            let backend = self.cache.backend().clone();
            let db = self.db.clone();
            let blob = blob_id.to_string();
            tasks.spawn(async move {
                let _permit = permit;
                let salt: [u8; 8] = random_bytes();
                let plain_len = data.len() as i64;
                let sealed = tokio::task::spawn_blocking(move || cipher.encrypt_chunk(&salt, &data))
                    .await
                    .map_err(|e| ApiError::Internal(e.to_string()))??;
                let name = format!("{}.bin", new_id());
                let locator = backend.put(&name, Bytes::from(sealed)).await.map_err(|e| ApiError::Backend(format!("{e:#}")))?;
                let kind = backend.kind().to_string();
                db.call(move |c| {
                    c.execute(
                        "INSERT INTO chunks (blob_id, part, seq, plain_len, salt, backend, locator) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![blob, part, seq, plain_len, salt.to_vec(), kind, locator],
                    )?;
                    Ok(())
                })
                .await
            });
        };

        while let Some(next) = body.next().await {
            let piece = match next {
                Ok(p) => p,
                Err(e) => {
                    failure = Some(ApiError::bad(format!("upload stream interrupted: {e}")));
                    break;
                }
            };
            total += piece.len() as u64;
            if total > limit {
                failure = Some(ApiError::PayloadTooLarge(format!("upload exceeds the {limit}-byte limit")));
                break;
            }
            hasher.update(&piece);
            let mut rest = piece;
            while !rest.is_empty() {
                let take = (self.chunk_size - buf.len()).min(rest.len());
                buf.extend_from_slice(&rest.split_to(take));
                if buf.len() == self.chunk_size {
                    let full = std::mem::replace(&mut buf, BytesMut::with_capacity(self.chunk_size)).freeze();
                    match self.slot().await {
                        Ok(permit) => flush(full, seq, permit, &mut tasks),
                        Err(e) => {
                            failure = Some(e);
                            break;
                        }
                    }
                    seq += 1;
                }
            }
            // Surface backend failures early instead of after the whole body arrived.
            while let Some(done) = tasks.try_join_next() {
                if let Err(e) = done.map_err(|e| ApiError::Internal(e.to_string())).and_then(|r| r) {
                    failure.get_or_insert(e);
                }
            }
            if failure.is_some() {
                break;
            }
        }
        if failure.is_none() && !buf.is_empty() {
            match self.slot().await {
                Ok(permit) => flush(buf.freeze(), seq, permit, &mut tasks),
                Err(e) => failure = Some(e),
            }
        }
        while let Some(done) = tasks.join_next().await {
            if let Err(e) = done.map_err(|e| ApiError::Internal(e.to_string())).and_then(|r| r) {
                failure.get_or_insert(e);
            }
        }
        match failure {
            Some(e) => Err(e),
            None => Ok((total, hex::encode(hasher.finalize()))),
        }
    }

    /// Assign global order and offsets to every chunk of a blob; returns total size.
    async fn seal_layout(&self, blob_id: &str) -> ApiResult<u64> {
        let bid = blob_id.to_string();
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                let rows: Vec<(i64, i64, i64)> = {
                    let mut st = tx.prepare("SELECT part, seq, plain_len FROM chunks WHERE blob_id = ?1 ORDER BY part, seq")?;
                    let it = st.query_map([&bid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                    it.collect::<Result<_, _>>()?
                };
                let mut offset = 0i64;
                for (ord, (part, seq, len)) in rows.iter().enumerate() {
                    tx.execute(
                        "UPDATE chunks SET ord = ?1, offset = ?2 WHERE blob_id = ?3 AND part = ?4 AND seq = ?5",
                        params![ord as i64, offset, bid, part, seq],
                    )?;
                    offset += len;
                }
                tx.commit()?;
                Ok(offset as u64)
            })
            .await
    }

    /// Mark a blob that failed to upload as garbage.
    pub async fn abandon_blob(&self, blob_id: &str) {
        let bid = blob_id.to_string();
        let _ = self
            .db
            .call(move |c| {
                c.execute("UPDATE blobs SET state = 'orphan' WHERE id = ?1 AND refcount = 0", [bid])?;
                Ok(())
            })
            .await;
    }

    /// Link a finished blob to a live object key. An existing object at the same key
    /// moves to the trash, which doubles as a simple previous-version history.
    pub async fn commit_object(&self, blob_id: &str, size: u64, sha256: Option<String>, etag: String, obj: NewObject) -> ApiResult<Object> {
        let id = new_id();
        let bid = blob_id.to_string();
        let oid = id.clone();
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                let t = now();
                check_quota(&tx, &obj.bucket_id, size as i64)?;
                tx.execute(
                    "UPDATE objects SET deleted_at = ?1 WHERE bucket_id = ?2 AND key = ?3 AND deleted_at IS NULL",
                    params![t, obj.bucket_id, obj.key],
                )?;
                tx.execute(
                    "UPDATE blobs SET state = 'ready', size = ?1, sha256 = COALESCE(?2, sha256), refcount = refcount + 1 WHERE id = ?3",
                    params![size as i64, sha256, bid],
                )?;
                tx.execute(
                    "INSERT INTO objects (id, bucket_id, key, blob_id, size, content_type, etag, meta, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![oid, obj.bucket_id, obj.key, bid, size as i64, obj.content_type, etag, obj.meta.to_string(), t, obj.expires_at],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.object_by_id(&id).await?.ok_or_else(|| ApiError::Internal("object vanished after insert".into()))
    }

    pub async fn put_object<S, E>(&self, obj: NewObject, body: S, quota_left: Option<u64>) -> ApiResult<Object>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let limit = quota_left.map_or(self.max_upload, |q| q.min(self.max_upload));
        let (blob_id, dek) = self.create_blob().await?;
        let (size, sha) = match self.write_part(&blob_id, dek, 0, body, limit).await {
            Ok(v) => v,
            Err(e) => {
                self.abandon_blob(&blob_id).await;
                return Err(e);
            }
        };
        if let Err(e) = self.seal_layout(&blob_id).await {
            self.abandon_blob(&blob_id).await;
            return Err(e);
        }
        let etag = sha[..32].to_string();
        match self.commit_object(&blob_id, size, Some(sha), etag, obj).await {
            Ok(o) => Ok(o),
            Err(e) => {
                self.abandon_blob(&blob_id).await;
                Err(e)
            }
        }
    }

    pub async fn complete_multipart(&self, blob_id: &str, obj: NewObject) -> ApiResult<Object> {
        let bid = blob_id.to_string();
        let parts: Vec<i64> = self
            .db
            .call(move |c| {
                let mut st = c.prepare("SELECT DISTINCT part FROM chunks WHERE blob_id = ?1 ORDER BY part")?;
                let v = st.query_map([bid], |r| r.get(0))?.collect::<Result<_, _>>()?;
                Ok(v)
            })
            .await?;
        if parts.is_empty() {
            return Err(ApiError::bad("upload has no parts"));
        }
        if let Some((i, p)) = parts.iter().enumerate().find(|(i, p)| **p != *i as i64 + 1) {
            return Err(ApiError::bad(format!("part {} is missing (found part {p} at position {i})", i + 1)));
        }
        let size = self.seal_layout(blob_id).await?;
        if size > self.max_upload {
            return Err(ApiError::PayloadTooLarge("assembled object exceeds TGV_MAX_UPLOAD_GIB".into()));
        }
        let etag = format!("{}-{}", &blob_id[..24], parts.len());
        self.commit_object(blob_id, size, None, etag, obj).await
    }

    /// Re-uploading a part replaces it: queue the old chunks for deletion first.
    pub async fn reset_part(&self, blob_id: &str, part: i64) -> ApiResult<()> {
        let bid = blob_id.to_string();
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute(
                    "INSERT INTO dead_chunks (backend, locator) SELECT backend, locator FROM chunks WHERE blob_id = ?1 AND part = ?2",
                    params![bid, part],
                )?;
                tx.execute("DELETE FROM chunks WHERE blob_id = ?1 AND part = ?2", params![bid, part])?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Create a new object that shares an existing blob (copy, or upload by hash).
    pub async fn link_blob(&self, blob_id: &str, size: i64, etag: String, obj: NewObject) -> ApiResult<Object> {
        let id = new_id();
        let (bid, oid) = (blob_id.to_string(), id.clone());
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                let t = now();
                let n = tx.execute("UPDATE blobs SET refcount = refcount + 1 WHERE id = ?1 AND state = 'ready'", [&bid])?;
                if n == 0 {
                    return Err(ApiError::Conflict("source data is no longer available".into()));
                }
                check_quota(&tx, &obj.bucket_id, size)?;
                tx.execute(
                    "UPDATE objects SET deleted_at = ?1 WHERE bucket_id = ?2 AND key = ?3 AND deleted_at IS NULL",
                    params![t, obj.bucket_id, obj.key],
                )?;
                tx.execute(
                    "INSERT INTO objects (id, bucket_id, key, blob_id, size, content_type, etag, meta, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![oid, obj.bucket_id, obj.key, bid, size, obj.content_type, etag, obj.meta.to_string(), t, obj.expires_at],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.object_by_id(&id).await?.ok_or_else(|| ApiError::Internal("object vanished after insert".into()))
    }

    pub async fn object_by_id(&self, id: &str) -> ApiResult<Option<Object>> {
        let id = id.to_string();
        self.db
            .call(move |c| {
                let sql = format!("SELECT {} FROM objects o JOIN buckets b ON b.id = o.bucket_id WHERE o.id = ?1", Object::COLS);
                Ok(c.query_row(&sql, [id], Object::from_row).optional()?)
            })
            .await
    }

    pub async fn live_object(&self, bucket_id: &str, key: &str) -> ApiResult<Option<Object>> {
        let (bid, key) = (bucket_id.to_string(), key.to_string());
        self.db
            .call(move |c| {
                let sql = format!(
                    "SELECT {} FROM objects o JOIN buckets b ON b.id = o.bucket_id
                     WHERE o.bucket_id = ?1 AND o.key = ?2 AND o.deleted_at IS NULL AND (o.expires_at IS NULL OR o.expires_at > ?3)",
                    Object::COLS
                );
                Ok(c.query_row(&sql, params![bid, key, now()], Object::from_row).optional()?)
            })
            .await
    }

    /// Soft delete (trash) or permanent delete. Permanent delete drops the blob
    /// reference; the GC removes chunks once nothing points at them.
    pub async fn delete_object(&self, object_id: &str, permanent: bool) -> ApiResult<bool> {
        let oid = object_id.to_string();
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                let n = if permanent {
                    let blob: Option<String> = tx.query_row("SELECT blob_id FROM objects WHERE id = ?1", [&oid], |r| r.get(0)).optional()?;
                    let Some(blob) = blob else { return Ok(false) };
                    tx.execute("DELETE FROM objects WHERE id = ?1", [&oid])?;
                    tx.execute("UPDATE blobs SET refcount = refcount - 1 WHERE id = ?1", [&blob])?;
                    tx.execute("UPDATE blobs SET state = 'orphan' WHERE id = ?1 AND refcount <= 0", [&blob])?;
                    1
                } else {
                    tx.execute("UPDATE objects SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL", params![now(), oid])?
                };
                tx.commit()?;
                Ok(n > 0)
            })
            .await
    }

    pub async fn restore_object(&self, object_id: &str) -> ApiResult<()> {
        let oid = object_id.to_string();
        self.db
            .call(move |c| {
                let n = c.execute("UPDATE objects SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL", [oid]).map_err(|e| {
                    match ApiError::from(e) {
                        ApiError::Conflict(_) => ApiError::Conflict("a live object already uses this key; delete or rename it first".into()),
                        other => other,
                    }
                })?;
                if n == 0 {
                    return Err(ApiError::not_found("trashed object"));
                }
                Ok(())
            })
            .await
    }

    async fn chunks_for(&self, blob_id: &str, start: u64, end: u64) -> ApiResult<Vec<ChunkRow>> {
        let bid = blob_id.to_string();
        self.db
            .call(move |c| {
                let mut st = c.prepare(
                    "SELECT offset, plain_len, salt, locator FROM chunks
                     WHERE blob_id = ?1 AND offset < ?3 AND offset + plain_len > ?2 ORDER BY ord",
                )?;
                let rows = st
                    .query_map(params![bid, start as i64, end as i64], |r| {
                        let salt: Vec<u8> = r.get(2)?;
                        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, salt, r.get::<_, String>(3)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.into_iter()
                    .map(|(o, l, s, loc)| {
                        let salt: [u8; 8] = s.try_into().map_err(|_| ApiError::Internal("corrupt chunk salt".into()))?;
                        Ok(ChunkRow { offset: o as u64, plain_len: l as u64, salt, locator: loc })
                    })
                    .collect()
            })
            .await
    }

    /// Lazily decrypting stream of plaintext bytes `[start, end)` of an object.
    pub async fn read_range(&self, obj: &Object, start: u64, end: u64) -> ApiResult<impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static> {
        let chunks = if start < end { self.chunks_for(&obj.blob_id, start, end).await? } else { vec![] };
        let covered: u64 = chunks.iter().map(|c| c.plain_len).sum();
        if start < end && (chunks.is_empty() || chunks[0].offset > start || covered < end - start) {
            // Defensive: the chunk map must cover the requested range exactly.
            let first = chunks.first().map(|c| c.offset).unwrap_or(0);
            if chunks.is_empty() || first > start || chunks.last().map(|c| c.offset + c.plain_len).unwrap_or(0) < end {
                return Err(ApiError::Internal(format!("chunk map for blob {} does not cover {start}..{end}", obj.blob_id)));
            }
        }
        let cipher = Arc::new(ChunkCipher::new(&self.blob_dek(&obj.blob_id).await?, &obj.blob_id));
        let cache = self.cache.clone();
        let chunks = Arc::new(chunks);
        let stream = futures::stream::unfold(0usize, move |i| {
            let (cache, cipher, chunks) = (cache.clone(), cipher.clone(), chunks.clone());
            async move {
                let ch = chunks.get(i)?.clone();
                if let Some(next) = chunks.get(i + 1) {
                    cache.prefetch(next.locator.clone());
                }
                let res = async {
                    let sealed = cache.get(&ch.locator).await.map_err(std::io::Error::other)?;
                    let from = start.saturating_sub(ch.offset) as usize;
                    let to = (end.min(ch.offset + ch.plain_len) - ch.offset) as usize;
                    let plain_len = ch.plain_len as usize;
                    let salt = ch.salt;
                    let out = tokio::task::spawn_blocking(move || cipher.decrypt_range(&salt, &sealed, plain_len, from, to))
                        .await
                        .map_err(std::io::Error::other)?
                        .map_err(|e| std::io::Error::other(format!("{e:#}")));
                    if out.is_err() {
                        cache.forget(&ch.locator);
                    }
                    out.map(Bytes::from)
                }
                .await;
                if let Err(e) = &res {
                    tracing::error!(error = %e, "chunk read failed");
                }
                Some((res, i + 1))
            }
        });
        Ok(stream)
    }

    pub async fn bucket_usage(&self, bucket_id: &str) -> ApiResult<i64> {
        let bid = bucket_id.to_string();
        self.db
            .call(move |c| Ok(c.query_row("SELECT COALESCE(SUM(size), 0) FROM objects WHERE bucket_id = ?1", [bid], |r| r.get(0))?))
            .await
    }
}
