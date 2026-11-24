use super::serve::{serve_object, ServeOpts};
use super::{bucket_by_name, guess_type, validate_key};
use crate::auth::{Principal, Scope};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::storage::{Bucket, NewObject, Object};
use crate::AppState;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

const MAX_META_BYTES: usize = 2048;

pub fn user_meta(headers: &HeaderMap) -> ApiResult<Value> {
    let mut m = Map::new();
    for (k, v) in headers {
        if let Some(name) = k.as_str().strip_prefix("x-tgv-meta-") {
            let v = v.to_str().map_err(|_| ApiError::bad("metadata values must be visible ASCII"))?;
            m.insert(name.to_string(), Value::String(v.to_string()));
        }
    }
    let v = Value::Object(m);
    if v.to_string().len() > MAX_META_BYTES {
        return Err(ApiError::bad(format!("user metadata is limited to {MAX_META_BYTES} bytes")));
    }
    Ok(v)
}

pub fn ttl_from(headers: &HeaderMap, bucket: &Bucket) -> ApiResult<Option<i64>> {
    let ttl = match headers.get("x-tgv-ttl") {
        Some(v) => Some(
            v.to_str()
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|t| *t > 0 && *t <= 100 * 365 * 86400)
                .ok_or_else(|| ApiError::bad("x-tgv-ttl must be a positive number of seconds"))?,
        ),
        None => bucket.default_ttl,
    };
    Ok(ttl.map(|t| now() + t))
}

/// Remaining quota for a bucket, or None when unlimited.
pub async fn quota_left(state: &AppState, b: &Bucket) -> ApiResult<Option<u64>> {
    match b.quota_bytes {
        None => Ok(None),
        Some(q) => {
            let used = state.engine.bucket_usage(&b.id).await?;
            Ok(Some((q - used).max(0) as u64))
        }
    }
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers.get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok())
}

pub async fn upload(
    State(state): State<AppState>,
    p: Principal,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<(StatusCode, Json<Object>)> {
    p.require_bucket(Scope::Write, &bucket)?;
    validate_key(&key)?;
    let b = bucket_by_name(&state, &bucket).await?;
    if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some("*") && state.engine.live_object(&b.id, &key).await?.is_some() {
        return Err(ApiError::Conflict("an object already exists at this key (If-None-Match: *)".into()));
    }
    let left = quota_left(&state, &b).await?;
    if let (Some(left), Some(len)) = (left, content_length(&headers)) {
        if len > left {
            return Err(ApiError::PayloadTooLarge(format!("bucket quota exceeded: {left} bytes left")));
        }
    }
    if content_length(&headers).is_some_and(|l| l > state.engine.max_upload) {
        return Err(ApiError::PayloadTooLarge("use a multipart upload or raise TGV_MAX_UPLOAD_GIB".into()));
    }
    let obj = NewObject {
        bucket_id: b.id.clone(),
        key: key.clone(),
        content_type: guess_type(&key, headers.get("content-type").and_then(|v| v.to_str().ok())),
        meta: user_meta(&headers)?,
        expires_at: ttl_from(&headers, &b)?,
    };
    let created = state.engine.put_object(obj, body.into_data_stream(), left).await?;
    state
        .db
        .audit(&p.actor, "object.put", &format!("{bucket}/{key}"), &p.ip.to_string(), Some(format!("{} bytes", created.size)))
        .await;
    Ok((StatusCode::CREATED, Json(created)))
}

#[derive(Deserialize, Default)]
pub struct DownloadQuery {
    #[serde(default)]
    pub download: bool,
}

pub async fn download(
    State(state): State<AppState>,
    p: Principal,
    method: Method,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<DownloadQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    p.require_bucket(Scope::Read, &bucket)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let obj = state.engine.live_object(&b.id, &key).await?.ok_or_else(|| ApiError::not_found("object"))?;
    serve_object(&state, &obj, &method, &headers, ServeOpts { force_download: q.download, cache_control: "private, max-age=0, must-revalidate" }).await
}

#[derive(Deserialize, Default)]
pub struct DeleteQuery {
    #[serde(default)]
    pub permanent: bool,
}

pub async fn remove(
    State(state): State<AppState>,
    p: Principal,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<StatusCode> {
    p.require_bucket(Scope::Delete, &bucket)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let obj = state.engine.live_object(&b.id, &key).await?.ok_or_else(|| ApiError::not_found("object"))?;
    state.engine.delete_object(&obj.id, q.permanent).await?;
    let action = if q.permanent { "object.delete" } else { "object.trash" };
    state.db.audit(&p.actor, action, &format!("{bucket}/{key}"), &p.ip.to_string(), None).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub prefix: String,
    pub delimiter: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

pub async fn list(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    p.require_bucket(Scope::List, &bucket)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let delimiter = q.delimiter.filter(|d| !d.is_empty());
    if delimiter.as_ref().is_some_and(|d| d.len() > 8) {
        return Err(ApiError::bad("delimiter is limited to 8 bytes"));
    }
    let prefix = q.prefix;
    let cursor = q.cursor.unwrap_or_default();
    let bid = b.id.clone();

    let (objects, prefixes, next): (Vec<Object>, Vec<String>, Option<String>) = state
        .db
        .call(move |c| {
            // substr() comparison instead of LIKE: no wildcard injection via the prefix.
            let sql = format!(
                "SELECT {} FROM objects o JOIN buckets b ON b.id = o.bucket_id
                 WHERE o.bucket_id = ?1 AND o.deleted_at IS NULL AND (o.expires_at IS NULL OR o.expires_at > ?2)
                   AND substr(o.key, 1, ?3) = ?4 AND o.key > ?5
                 ORDER BY o.key LIMIT ?6",
                Object::COLS
            );
            let mut st = c.prepare(&sql)?;
            let plen = prefix.chars().count() as i64;
            let mut after = cursor;
            let mut out = Vec::new();
            let mut cps = BTreeSet::new();
            let t = now();
            loop {
                let batch: Vec<Object> = st
                    .query_map(params![bid, t, plen, prefix, after, 500], Object::from_row)?
                    .collect::<Result<_, _>>()?;
                if batch.is_empty() {
                    return Ok((out, cps.into_iter().collect(), None));
                }
                for o in batch {
                    if out.len() + cps.len() >= limit {
                        return Ok((out, cps.into_iter().collect(), Some(after)));
                    }
                    let rest = &o.key[prefix.len()..];
                    if let Some(d) = delimiter.as_deref() {
                        if let Some(pos) = rest.find(d) {
                            let cp = format!("{prefix}{}", &rest[..pos + d.len()]);
                            // Skip every key under this common prefix in one step.
                            after = format!("{cp}\u{10FFFF}");
                            cps.insert(cp);
                            break;
                        }
                    }
                    after = o.key.clone();
                    out.push(o);
                }
            }
        })
        .await?;
    Ok(Json(json!({ "objects": objects, "common_prefixes": prefixes, "next_cursor": next })))
}

#[derive(Deserialize)]
pub struct CopyReq {
    pub source_key: String,
    pub source_bucket: Option<String>,
    pub dest_key: String,
}

pub async fn copy(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>, Json(req): Json<CopyReq>) -> ApiResult<(StatusCode, Json<Object>)> {
    let src_bucket = req.source_bucket.clone().unwrap_or_else(|| bucket.clone());
    p.require_bucket(Scope::Read, &src_bucket)?;
    p.require_bucket(Scope::Write, &bucket)?;
    validate_key(&req.dest_key)?;
    let sb = bucket_by_name(&state, &src_bucket).await?;
    let db_ = bucket_by_name(&state, &bucket).await?;
    let src = state.engine.live_object(&sb.id, &req.source_key).await?.ok_or_else(|| ApiError::not_found("source object"))?;
    if quota_left(&state, &db_).await?.is_some_and(|l| (src.size as u64) > l) {
        return Err(ApiError::PayloadTooLarge("bucket quota exceeded".into()));
    }
    let obj = NewObject { bucket_id: db_.id.clone(), key: req.dest_key.clone(), content_type: src.content_type.clone(), meta: src.meta.clone(), expires_at: src.expires_at };
    let created = state.engine.link_blob(&src.blob_id, src.size, src.etag.clone(), obj).await?;
    state
        .db
        .audit(&p.actor, "object.copy", &format!("{src_bucket}/{} -> {bucket}/{}", req.source_key, req.dest_key), &p.ip.to_string(), None)
        .await;
    Ok((StatusCode::CREATED, Json(created)))
}

#[derive(Deserialize)]
pub struct LinkReq {
    pub key: String,
    pub sha256: String,
    pub content_type: Option<String>,
}

/// "Instant upload": if this bucket already stores bytes with the given SHA-256,
/// create the new key without sending the data again. Scoped to one bucket on
/// purpose: a hash alone must not grant access to someone else's file.
pub async fn link_by_hash(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>, Json(req): Json<LinkReq>) -> ApiResult<(StatusCode, Json<Object>)> {
    p.require_bucket(Scope::Write, &bucket)?;
    // Without read access the 201/404 answer would be an oracle for "this bucket
    // contains a file with hash X".
    p.require_bucket(Scope::Read, &bucket)?;
    validate_key(&req.key)?;
    let sha = req.sha256.to_ascii_lowercase();
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad("sha256 must be 64 hex characters"));
    }
    let b = bucket_by_name(&state, &bucket).await?;
    let bid = b.id.clone();
    let found: Option<(String, i64, String, String)> = state
        .db
        .call(move |c| {
            Ok(c.query_row(
                "SELECT o.blob_id, o.size, o.etag, o.content_type FROM objects o JOIN blobs bl ON bl.id = o.blob_id
                 WHERE o.bucket_id = ?1 AND bl.sha256 = ?2 AND bl.state = 'ready' LIMIT 1",
                params![bid, sha],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?)
        })
        .await?;
    let (blob, size, etag, ct) = found.ok_or_else(|| ApiError::NotFound("no stored object in this bucket has that hash; upload it normally".into()))?;
    if quota_left(&state, &b).await?.is_some_and(|l| (size as u64) > l) {
        return Err(ApiError::PayloadTooLarge("bucket quota exceeded".into()));
    }
    let obj = NewObject {
        bucket_id: b.id.clone(),
        key: req.key.clone(),
        content_type: req.content_type.map(|c| guess_type(&req.key, Some(&c))).unwrap_or(ct),
        meta: json!({}),
        expires_at: b.default_ttl.map(|t| now() + t),
    };
    let created = state.engine.link_blob(&blob, size, etag, obj).await?;
    state.db.audit(&p.actor, "object.link_hash", &format!("{bucket}/{}", req.key), &p.ip.to_string(), None).await;
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn trash(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>) -> ApiResult<Json<Value>> {
    p.require_bucket(Scope::List, &bucket)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let bid = b.id.clone();
    let items: Vec<Object> = state
        .db
        .call(move |c| {
            let sql = format!(
                "SELECT {} FROM objects o JOIN buckets b ON b.id = o.bucket_id WHERE o.bucket_id = ?1 AND o.deleted_at IS NOT NULL ORDER BY o.deleted_at DESC LIMIT 500",
                Object::COLS
            );
            let mut st = c.prepare(&sql)?;
            let v = st.query_map([bid], Object::from_row)?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({ "objects": items, "purge_after_days": state.cfg.trash_days })))
}

pub async fn empty_trash(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>) -> ApiResult<Json<Value>> {
    p.require_bucket(Scope::Delete, &bucket)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let bid = b.id.clone();
    let ids: Vec<String> = state
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT id FROM objects WHERE bucket_id = ?1 AND deleted_at IS NOT NULL")?;
            let v = st.query_map([bid], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    let n = ids.len();
    for id in ids {
        state.engine.delete_object(&id, true).await?;
    }
    state.db.audit(&p.actor, "trash.empty", &bucket, &p.ip.to_string(), Some(format!("{n} objects"))).await;
    Ok(Json(json!({ "deleted": n })))
}

async fn object_checked(state: &AppState, p: &Principal, id: &str, scope: Scope) -> ApiResult<Object> {
    let obj = state.engine.object_by_id(id).await?.ok_or_else(|| ApiError::not_found("object"))?;
    // Same 404 for "exists elsewhere" and "missing" so ids cannot be probed across buckets.
    if p.require_bucket(scope, &obj.bucket).is_err() {
        p.require(scope)?;
        return Err(ApiError::not_found("object"));
    }
    Ok(obj)
}

pub async fn by_id(State(state): State<AppState>, p: Principal, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let obj = object_checked(&state, &p, &id, Scope::Read).await?;
    let bid = obj.blob_id.clone();
    let (chunks, refs): (Vec<Value>, i64) = state
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT ord, offset, plain_len, backend, locator FROM chunks WHERE blob_id = ?1 ORDER BY ord")?;
            let v: Vec<Value> = st
                .query_map([&bid], |r| {
                    let loc: String = r.get(4)?;
                    // Expose which bot holds the chunk, never file ids or message ids.
                    let bot = serde_json::from_str::<Value>(&loc).ok().and_then(|l| l.get("b").cloned());
                    Ok(json!({ "index": r.get::<_, Option<i64>>(0)?, "offset": r.get::<_, Option<i64>>(1)?, "size": r.get::<_, i64>(2)?, "backend": r.get::<_, String>(3)?, "bot": bot }))
                })?
                .collect::<Result<_, _>>()?;
            let refs = c.query_row("SELECT refcount FROM blobs WHERE id = ?1", [&bid], |r| r.get(0))?;
            Ok((v, refs))
        })
        .await?;
    let mut v = serde_json::to_value(&obj).unwrap();
    v["chunks"] = json!(chunks);
    v["shared_by"] = json!(refs);
    Ok(Json(v))
}

pub async fn restore(State(state): State<AppState>, p: Principal, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let obj = object_checked(&state, &p, &id, Scope::Write).await?;
    state.engine.restore_object(&obj.id).await?;
    state.db.audit(&p.actor, "object.restore", &format!("{}/{}", obj.bucket, obj.key), &p.ip.to_string(), None).await;
    Ok(StatusCode::NO_CONTENT)
}
