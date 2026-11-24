//! S3-style multipart upload: init -> PUT parts (any order, retryable) -> complete.
//! Lets browsers and scripts send multi-GB files in resumable pieces.

use super::objects::{quota_left, ttl_from};
use super::{bucket_by_name, guess_type, validate_key};
use crate::auth::{Principal, Scope};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::storage::{new_id, NewObject, Object};
use crate::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

pub const MAX_PARTS: i64 = 10_000;
/// Uploads not completed within a day are aborted by the GC.
pub const UPLOAD_TTL_SECS: i64 = 24 * 3600;

#[derive(Deserialize)]
pub struct InitReq {
    pub key: String,
    pub content_type: Option<String>,
}

struct Upload {
    id: String,
    bucket_id: String,
    bucket: String,
    key: String,
    blob_id: String,
    content_type: String,
    owner: String,
    created_at: i64,
}

async fn load(state: &AppState, p: &Principal, id: &str) -> ApiResult<Upload> {
    let id = id.to_string();
    let up = state
        .db
        .call(move |c| {
            Ok(c.query_row(
                "SELECT u.id, u.bucket_id, b.name, u.key, u.blob_id, u.content_type, u.owner, u.created_at
                 FROM uploads u JOIN buckets b ON b.id = u.bucket_id WHERE u.id = ?1",
                [id],
                |r| {
                    Ok(Upload {
                        id: r.get(0)?,
                        bucket_id: r.get(1)?,
                        bucket: r.get(2)?,
                        key: r.get(3)?,
                        blob_id: r.get(4)?,
                        content_type: r.get(5)?,
                        owner: r.get(6)?,
                        created_at: r.get(7)?,
                    })
                },
            )
            .optional()?)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("upload"))?;
    // Only the key that started an upload (or an admin) may touch it.
    if !(p.is_admin() || up.owner == p.actor) || p.require_bucket(Scope::Write, &up.bucket).is_err() {
        return Err(ApiError::not_found("upload"));
    }
    Ok(up)
}

pub async fn init(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>, Json(req): Json<InitReq>) -> ApiResult<(StatusCode, Json<Value>)> {
    p.require_bucket(Scope::Write, &bucket)?;
    validate_key(&req.key)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let (blob_id, _dek) = state.engine.create_blob().await?;
    let id = new_id();
    let ct = guess_type(&req.key, req.content_type.as_deref());
    let (uid, bid, key, owner, blob) = (id.clone(), b.id.clone(), req.key.clone(), p.actor.clone(), blob_id.clone());
    state
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO uploads (id, bucket_id, key, blob_id, content_type, owner, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![uid, bid, key, blob, ct, owner, now()],
            )?;
            Ok(())
        })
        .await?;
    state.db.audit(&p.actor, "upload.init", &format!("{bucket}/{}", req.key), &p.ip.to_string(), None).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "upload_id": id, "bucket": bucket, "key": req.key, "chunk_size": state.engine.chunk_size, "max_parts": MAX_PARTS, "expires_at": now() + UPLOAD_TTL_SECS })),
    ))
}

pub async fn upload_part(State(state): State<AppState>, p: Principal, Path((id, n)): Path<(String, i64)>, body: Body) -> ApiResult<Json<Value>> {
    if !(1..=MAX_PARTS).contains(&n) {
        return Err(ApiError::bad(format!("part numbers run from 1 to {MAX_PARTS}")));
    }
    let up = load(&state, &p, &id).await?;
    let b = bucket_by_name(&state, &up.bucket).await?;
    let blob = up.blob_id.clone();
    let already: i64 = state
        .db
        .call(move |c| Ok(c.query_row("SELECT COALESCE(SUM(plain_len), 0) FROM chunks WHERE blob_id = ?1 AND part != ?2", params![blob, n], |r| r.get(0))?))
        .await?;
    let mut limit = state.engine.max_upload.saturating_sub(already as u64);
    if let Some(left) = quota_left(&state, &b).await? {
        limit = limit.min(left.saturating_sub(already as u64));
    }
    state.engine.reset_part(&up.blob_id, n).await?;
    let dek = state.engine.blob_dek(&up.blob_id).await?;
    let (size, sha) = state.engine.write_part(&up.blob_id, dek, n, body.into_data_stream(), limit).await?;
    Ok(Json(json!({ "part": n, "size": size, "sha256": sha })))
}

pub async fn status(State(state): State<AppState>, p: Principal, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let up = load(&state, &p, &id).await?;
    let blob = up.blob_id.clone();
    let parts: Vec<Value> = state
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT part, SUM(plain_len), COUNT(*) FROM chunks WHERE blob_id = ?1 GROUP BY part ORDER BY part")?;
            let v = st
                .query_map([blob], |r| Ok(json!({ "part": r.get::<_, i64>(0)?, "size": r.get::<_, i64>(1)?, "chunks": r.get::<_, i64>(2)? })))?
                .collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({ "upload_id": up.id, "bucket": up.bucket, "key": up.key, "parts": parts, "expires_at": up.created_at + UPLOAD_TTL_SECS })))
}

pub async fn list(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>) -> ApiResult<Json<Value>> {
    p.require_bucket(Scope::Write, &bucket)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let (bid, owner, admin) = (b.id.clone(), p.actor.clone(), p.is_admin());
    let items: Vec<Value> = state
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT id, key, owner, created_at FROM uploads WHERE bucket_id = ?1 AND (?2 OR owner = ?3) ORDER BY created_at DESC")?;
            let v = st
                .query_map(params![bid, admin, owner], |r| {
                    Ok(json!({ "upload_id": r.get::<_, String>(0)?, "key": r.get::<_, String>(1)?, "owner": r.get::<_, String>(2)?, "created_at": r.get::<_, i64>(3)? }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({ "uploads": items })))
}

pub async fn complete(State(state): State<AppState>, p: Principal, Path(id): Path<String>, headers: HeaderMap) -> ApiResult<(StatusCode, Json<Object>)> {
    let up = load(&state, &p, &id).await?;
    let b = bucket_by_name(&state, &up.bucket).await?;
    let obj = NewObject {
        bucket_id: up.bucket_id.clone(),
        key: up.key.clone(),
        content_type: up.content_type.clone(),
        meta: super::objects::user_meta(&headers)?,
        expires_at: ttl_from(&headers, &b)?,
    };
    let created = state.engine.complete_multipart(&up.blob_id, obj).await?;
    let uid = up.id.clone();
    state
        .db
        .call(move |c| {
            c.execute("DELETE FROM uploads WHERE id = ?1", [uid])?;
            Ok(())
        })
        .await?;
    state
        .db
        .audit(&p.actor, "upload.complete", &format!("{}/{}", up.bucket, up.key), &p.ip.to_string(), Some(format!("{} bytes", created.size)))
        .await;
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn abort(State(state): State<AppState>, p: Principal, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let up = load(&state, &p, &id).await?;
    abort_upload(&state, &up.id, &up.blob_id).await?;
    state.db.audit(&p.actor, "upload.abort", &format!("{}/{}", up.bucket, up.key), &p.ip.to_string(), None).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn abort_upload(state: &AppState, upload_id: &str, blob_id: &str) -> ApiResult<()> {
    let (uid, blob) = (upload_id.to_string(), blob_id.to_string());
    state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM uploads WHERE id = ?1", [&uid])?;
            tx.execute("UPDATE blobs SET state = 'orphan' WHERE id = ?1 AND refcount = 0", [&blob])?;
            tx.commit()?;
            Ok(())
        })
        .await
}
