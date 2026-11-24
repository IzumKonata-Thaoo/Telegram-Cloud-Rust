use crate::auth::{Principal, Scope};
use crate::error::ApiResult;
use crate::AppState;
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn healthz(State(state): State<AppState>) -> Json<Value> {
    let db_ok = state.db.call(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)).await.is_ok();
    Json(json!({ "ok": db_ok, "version": env!("CARGO_PKG_VERSION") }))
}

pub async fn stats(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::Admin)?;
    let counts: Value = state
        .db
        .call(|c| {
            let one = |sql: &str| -> rusqlite::Result<i64> { c.query_row(sql, [], |r| r.get(0)) };
            Ok(json!({
                "buckets": one("SELECT COUNT(*) FROM buckets")?,
                "objects": one("SELECT COUNT(*) FROM objects WHERE deleted_at IS NULL")?,
                "trashed": one("SELECT COUNT(*) FROM objects WHERE deleted_at IS NOT NULL")?,
                "logical_bytes": one("SELECT COALESCE(SUM(size),0) FROM objects WHERE deleted_at IS NULL")?,
                "stored_bytes": one("SELECT COALESCE(SUM(size),0) FROM blobs WHERE state = 'ready'")?,
                "chunks": one("SELECT COUNT(*) FROM chunks")?,
                "pending_chunk_deletes": one("SELECT COUNT(*) FROM dead_chunks")?,
                "uploads_in_progress": one("SELECT COUNT(*) FROM uploads")?,
                "api_keys": one("SELECT COUNT(*) FROM api_keys WHERE revoked_at IS NULL")?,
                "shares": one("SELECT COUNT(*) FROM shares WHERE revoked_at IS NULL")?,
                "last_backup_at": c.query_row("SELECT MAX(created_at) FROM backups", [], |r| r.get::<_, Option<i64>>(0))?,
            }))
        })
        .await?;
    let per_bucket: Vec<Value> = state
        .db
        .call(|c| {
            let mut st = c.prepare(
                "SELECT b.name, COUNT(o.id), COALESCE(SUM(o.size),0) FROM buckets b
                 LEFT JOIN objects o ON o.bucket_id = b.id AND o.deleted_at IS NULL GROUP BY b.id ORDER BY 3 DESC",
            )?;
            let v = st
                .query_map([], |r| Ok(json!({ "name": r.get::<_, String>(0)?, "objects": r.get::<_, i64>(1)?, "bytes": r.get::<_, i64>(2)? })))?
                .collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    // Chunk distribution per bot (Telegram locators are JSON with "b"; local ones are not).
    let per_bot: Vec<Value> = state
        .db
        .call(|c| {
            let mut st = c.prepare(
                "SELECT CASE WHEN json_valid(locator) THEN json_extract(locator, '$.b') END AS bot, COUNT(*), COALESCE(SUM(plain_len), 0)
                 FROM chunks GROUP BY bot ORDER BY 2 DESC",
            )?;
            let v = st
                .query_map([], |r| Ok(json!({ "bot": r.get::<_, Option<i64>>(0)?, "chunks": r.get::<_, i64>(1)?, "bytes": r.get::<_, i64>(2)? })))?
                .collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({
        "totals": counts,
        "buckets": per_bucket,
        "chunk_bots": per_bot,
        "cache": state.engine.cache.stats(),
        "backend": state.backend.health().await,
        "uptime_secs": state.started.elapsed().as_secs(),
        "chunk_size": state.engine.chunk_size,
        "active_key": state.keys.active,
        "key_versions": state.keys.versions(),
    })))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<i64>,
    pub before: Option<i64>,
    pub actor: Option<String>,
}

pub async fn audit(State(state): State<AppState>, p: Principal, Query(q): Query<AuditQuery>) -> ApiResult<Json<Value>> {
    p.require(Scope::Admin)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let before = q.before.unwrap_or(i64::MAX);
    let actor = q.actor;
    let rows: Vec<Value> = state
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT id, ts, actor, action, target, ip, detail FROM audit WHERE id < ?1 AND (?2 IS NULL OR actor = ?2) ORDER BY id DESC LIMIT ?3",
            )?;
            let v = st
                .query_map(rusqlite::params![before, actor, limit], |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?, "ts": r.get::<_, i64>(1)?, "actor": r.get::<_, String>(2)?,
                        "action": r.get::<_, String>(3)?, "target": r.get::<_, String>(4)?, "ip": r.get::<_, String>(5)?,
                        "detail": r.get::<_, Option<String>>(6)?,
                    }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({ "events": rows })))
}

pub async fn list_backups(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::Admin)?;
    let rows: Vec<Value> = state
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT id, created_at, size, backend FROM backups ORDER BY created_at DESC LIMIT 100")?;
            let v = st
                .query_map([], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "created_at": r.get::<_, i64>(1)?, "size": r.get::<_, i64>(2)?, "backend": r.get::<_, String>(3)? })))?
                .collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({ "backups": rows, "interval_hours": state.cfg.backup_interval_hours })))
}

pub async fn backup_now(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::Admin)?;
    let r = crate::jobs::backup(&state).await?;
    state.db.audit(&p.actor, "backup.create", "metadata", &p.ip.to_string(), None).await;
    Ok(Json(r))
}

pub async fn gc_now(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::Admin)?;
    let r = crate::jobs::gc(&state).await?;
    state.db.audit(&p.actor, "gc.run", "storage", &p.ip.to_string(), Some(r.to_string())).await;
    Ok(Json(r))
}

pub async fn rewrap(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::Admin)?;
    let r = crate::jobs::rewrap(&state).await?;
    state.db.audit(&p.actor, "keys.rewrap", "blobs", &p.ip.to_string(), Some(r.to_string())).await;
    Ok(Json(r))
}
