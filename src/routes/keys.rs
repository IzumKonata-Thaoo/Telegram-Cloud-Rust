use crate::auth::{new_api_key, Principal, Scope};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CreateKey {
    pub name: String,
    pub scopes: Vec<Scope>,
    /// Bucket names this key may use. Omit for all buckets.
    pub buckets: Option<Vec<String>>,
    /// IPs or CIDR blocks the key may be used from. Omit for anywhere.
    pub allowed_ips: Option<Vec<String>>,
    /// Requests per minute. Omit for the server default; 0 disables the limit.
    pub rpm: Option<u32>,
    /// Lifetime in seconds. Omit for a key that does not expire.
    pub expires_in: Option<i64>,
}

pub async fn create(State(state): State<AppState>, p: Principal, Json(req): Json<CreateKey>) -> ApiResult<(StatusCode, Json<Value>)> {
    p.require(Scope::Admin)?;
    let name = req.name.trim().to_string();
    if name.is_empty() || name.len() > 80 {
        return Err(ApiError::bad("name must be 1-80 characters"));
    }
    if req.scopes.is_empty() {
        return Err(ApiError::bad("give the key at least one scope"));
    }
    if req.expires_in.is_some_and(|e| e <= 0) {
        return Err(ApiError::bad("expires_in must be positive"));
    }
    if let Some(ips) = &req.allowed_ips {
        if ips.is_empty() || ips.iter().any(|e| !ip_allowed_syntax(e)) {
            return Err(ApiError::bad("allowed_ips entries must be IP addresses or CIDR blocks"));
        }
    }
    if let Some(b) = &req.buckets {
        if b.is_empty() {
            return Err(ApiError::bad("buckets cannot be an empty list; omit it to allow all buckets"));
        }
        for name in b {
            super::validate_bucket_name(name)?;
        }
    }
    let (id, secret, full) = new_api_key();
    let hash = state.keys.hash_secret(&secret);
    let scopes = serde_json::to_string(&req.scopes).unwrap();
    let buckets = req.buckets.as_ref().map(|b| serde_json::to_string(b).unwrap());
    let ips = req.allowed_ips.as_ref().map(|b| serde_json::to_string(b).unwrap());
    let expires_at = req.expires_in.map(|e| now() + e);
    let (kid, kname) = (id.clone(), name.clone());
    state
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO api_keys (id, name, secret_hash, scopes, buckets, allowed_ips, rpm, expires_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![kid, kname, hash, scopes, buckets, ips, req.rpm, expires_at, now()],
            )?;
            Ok(())
        })
        .await?;
    state.db.audit(&p.actor, "key.create", &id, &p.ip.to_string(), Some(name.clone())).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "name": name,
            "key": full,
            "scopes": req.scopes,
            "buckets": req.buckets,
            "allowed_ips": req.allowed_ips,
            "expires_at": expires_at,
            "note": "Store this key now. Only a hash is kept on the server, so it cannot be shown again."
        })),
    ))
}

fn ip_allowed_syntax(entry: &str) -> bool {
    let (net, bits) = match entry.split_once('/') {
        Some((n, b)) => (n, Some(b)),
        None => (entry, None),
    };
    let Ok(ip) = net.parse::<std::net::IpAddr>() else { return false };
    let max = if ip.is_ipv4() { 32 } else { 128 };
    bits.is_none_or(|b| b.parse::<u32>().is_ok_and(|b| b <= max))
}

pub async fn list(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::Admin)?;
    let keys: Vec<Value> = state
        .db
        .call(|c| {
            let mut st = c.prepare(
                "SELECT id, name, scopes, buckets, allowed_ips, rpm, expires_at, revoked_at, last_used_at, created_at FROM api_keys ORDER BY created_at DESC",
            )?;
            let v = st
                .query_map([], |r| {
                    let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok());
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "name": r.get::<_, String>(1)?,
                        "scopes": parse(r.get(2)?),
                        "buckets": parse(r.get(3)?),
                        "allowed_ips": parse(r.get(4)?),
                        "rpm": r.get::<_, Option<i64>>(5)?,
                        "expires_at": r.get::<_, Option<i64>>(6)?,
                        "revoked_at": r.get::<_, Option<i64>>(7)?,
                        "last_used_at": r.get::<_, Option<i64>>(8)?,
                        "created_at": r.get::<_, i64>(9)?,
                    }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({ "keys": keys })))
}

pub async fn revoke(State(state): State<AppState>, p: Principal, Path(id): Path<String>) -> ApiResult<StatusCode> {
    p.require(Scope::Admin)?;
    let kid = id.clone();
    let n = state
        .db
        .call(move |c| Ok(c.execute("UPDATE api_keys SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL", params![now(), kid])?))
        .await?;
    if n == 0 {
        return Err(ApiError::not_found("active key"));
    }
    state.db.audit(&p.actor, "key.revoke", &id, &p.ip.to_string(), None).await;
    Ok(StatusCode::NO_CONTENT)
}
