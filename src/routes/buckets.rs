use super::{bucket_by_name, validate_bucket_name};
use crate::auth::{Principal, Scope};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::storage::{new_id, Bucket};
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CreateBucket {
    pub name: String,
    #[serde(default)]
    pub public: bool,
    #[serde(default)]
    pub cors_origins: Vec<String>,
    pub quota_bytes: Option<i64>,
    pub default_ttl: Option<i64>,
}

fn check_cors(origins: &[String]) -> ApiResult<()> {
    for o in origins {
        let ok = o == "*"
            || ((o.starts_with("https://") || o.starts_with("http://"))
                && !o.ends_with('/')
                && o.len() < 200
                && !o.contains(['?', '#', ' ', '"', '\'', '\n', '\r', ',']));
        if !ok {
            return Err(ApiError::bad(format!("'{o}' is not a valid CORS origin (scheme://host[:port], or *)")));
        }
    }
    Ok(())
}

fn check_limits(quota: Option<i64>, ttl: Option<i64>) -> ApiResult<()> {
    if quota.is_some_and(|q| q <= 0) {
        return Err(ApiError::bad("quota_bytes must be positive"));
    }
    if ttl.is_some_and(|t| t <= 0) {
        return Err(ApiError::bad("default_ttl must be a positive number of seconds"));
    }
    Ok(())
}

pub async fn list(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::List)?;
    let rows: Vec<(Bucket, i64, i64)> = state
        .db
        .call(|c| {
            let sql = format!(
                "SELECT {}, (SELECT COUNT(*) FROM objects o WHERE o.bucket_id = buckets.id AND o.deleted_at IS NULL),
                        (SELECT COALESCE(SUM(size),0) FROM objects o WHERE o.bucket_id = buckets.id)
                 FROM buckets ORDER BY name",
                Bucket::COLS
            );
            let mut st = c.prepare(&sql)?;
            let v = st.query_map([], |r| Ok((Bucket::from_row(r)?, r.get(7)?, r.get(8)?)))?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .filter(|(b, _, _)| p.can_see_bucket(&b.name))
        .map(|(b, n, bytes)| {
            let mut v = serde_json::to_value(&b).unwrap();
            v["objects"] = json!(n);
            v["bytes"] = json!(bytes);
            v
        })
        .collect();
    Ok(Json(json!({ "buckets": items })))
}

pub async fn create(State(state): State<AppState>, p: Principal, Json(req): Json<CreateBucket>) -> ApiResult<(StatusCode, Json<Bucket>)> {
    p.require(Scope::Admin)?;
    validate_bucket_name(&req.name)?;
    check_cors(&req.cors_origins)?;
    check_limits(req.quota_bytes, req.default_ttl)?;
    let id = new_id();
    let name = req.name.clone();
    let cors = serde_json::to_string(&req.cors_origins).unwrap();
    state
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO buckets (id, name, public, cors_origins, quota_bytes, default_ttl, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, req.name, req.public as i64, cors, req.quota_bytes, req.default_ttl, now()],
            )
            .map_err(|e| match ApiError::from(e) {
                ApiError::Conflict(_) => ApiError::Conflict("a bucket with this name already exists".into()),
                o => o,
            })?;
            Ok(())
        })
        .await?;
    state.db.audit(&p.actor, "bucket.create", &name, &p.ip.to_string(), None).await;
    Ok((StatusCode::CREATED, Json(bucket_by_name(&state, &name).await?)))
}

pub async fn get_one(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>) -> ApiResult<Json<Value>> {
    p.require_bucket(Scope::List, &bucket)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let used = state.engine.bucket_usage(&b.id).await?;
    let mut v = serde_json::to_value(&b).unwrap();
    v["bytes"] = json!(used);
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct UpdateBucket {
    pub public: Option<bool>,
    pub cors_origins: Option<Vec<String>>,
    /// Send `0` to remove the quota.
    pub quota_bytes: Option<i64>,
    /// Send `0` to remove the default TTL.
    pub default_ttl: Option<i64>,
}

pub async fn update(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>, Json(req): Json<UpdateBucket>) -> ApiResult<Json<Bucket>> {
    p.require(Scope::Admin)?;
    let b = bucket_by_name(&state, &bucket).await?;
    if let Some(c) = &req.cors_origins {
        check_cors(c)?;
    }
    check_limits(req.quota_bytes.filter(|q| *q != 0), req.default_ttl.filter(|t| *t != 0))?;
    let public = req.public.unwrap_or(b.public);
    let cors = serde_json::to_string(req.cors_origins.as_ref().unwrap_or(&b.cors_origins)).unwrap();
    let quota = match req.quota_bytes {
        Some(0) => None,
        Some(q) => Some(q),
        None => b.quota_bytes,
    };
    let ttl = match req.default_ttl {
        Some(0) => None,
        Some(t) => Some(t),
        None => b.default_ttl,
    };
    let id = b.id.clone();
    state
        .db
        .call(move |c| {
            c.execute(
                "UPDATE buckets SET public = ?1, cors_origins = ?2, quota_bytes = ?3, default_ttl = ?4 WHERE id = ?5",
                params![public as i64, cors, quota, ttl, id],
            )?;
            Ok(())
        })
        .await?;
    state.db.audit(&p.actor, "bucket.update", &bucket, &p.ip.to_string(), Some(format!("public={public}"))).await;
    Ok(Json(bucket_by_name(&state, &bucket).await?))
}

#[derive(Deserialize)]
pub struct RemoveQuery {
    #[serde(default)]
    pub force: bool,
}

pub async fn remove(State(state): State<AppState>, p: Principal, Path(bucket): Path<String>, Query(q): Query<RemoveQuery>) -> ApiResult<StatusCode> {
    p.require(Scope::Admin)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let bid = b.id.clone();
    let ids: Vec<String> = state
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT id FROM objects WHERE bucket_id = ?1")?;
            let v = st.query_map([bid], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    if !ids.is_empty() && !q.force {
        return Err(ApiError::Conflict(format!("bucket still holds {} objects (including trash); pass ?force=true to delete them", ids.len())));
    }
    for id in ids {
        state.engine.delete_object(&id, true).await?;
    }
    let bid = b.id.clone();
    state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            // Abort pending multipart uploads: their blobs become garbage.
            tx.execute("UPDATE blobs SET state = 'orphan' WHERE id IN (SELECT blob_id FROM uploads WHERE bucket_id = ?1)", [&bid])?;
            tx.execute("DELETE FROM uploads WHERE bucket_id = ?1", [&bid])?;
            tx.execute("DELETE FROM buckets WHERE id = ?1", [&bid])?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    state.db.audit(&p.actor, "bucket.delete", &bucket, &p.ip.to_string(), None).await;
    Ok(StatusCode::NO_CONTENT)
}
