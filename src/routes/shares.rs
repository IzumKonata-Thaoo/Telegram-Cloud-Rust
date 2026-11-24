//! Share links: an unguessable URL for one object, with optional password,
//! expiry and download cap. The landing page plays video/audio/images inline.

use super::serve::{serve_object, ServeOpts};
use super::bucket_by_name;
use crate::auth::{named_cookie, PublicClient, Principal, Scope};
use crate::crypto::{ct_eq, random_token};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::storage::{new_id, Object};
use crate::AppState;
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CreateShare {
    pub bucket: String,
    pub key: String,
    pub expires_in: Option<i64>,
    pub password: Option<String>,
    pub max_downloads: Option<i64>,
}

pub fn hash_password(pw: &str) -> ApiResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(format!("argon2: {e}")))
}

pub fn verify_password(pw: &str, phc: &str) -> bool {
    PasswordHash::new(phc).is_ok_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

/// Argon2 is deliberately expensive (~19 MiB, tens of ms). Cap concurrent checks so
/// a flood of login/unlock attempts from many IPs cannot exhaust CPU and memory.
static ARGON_SLOTS: std::sync::LazyLock<tokio::sync::Semaphore> = std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(4));

pub async fn verify_password_async(pw: String, phc: String) -> bool {
    let Ok(_permit) = ARGON_SLOTS.acquire().await else { return false };
    tokio::task::spawn_blocking(move || verify_password(&pw, &phc)).await.unwrap_or(false)
}

pub async fn create(State(state): State<AppState>, p: Principal, Json(req): Json<CreateShare>) -> ApiResult<(StatusCode, Json<Value>)> {
    p.require_bucket(Scope::Share, &req.bucket)?;
    p.require_bucket(Scope::Read, &req.bucket)?;
    if req.expires_in.is_some_and(|e| e <= 0) || req.max_downloads.is_some_and(|m| m <= 0) {
        return Err(ApiError::bad("expires_in and max_downloads must be positive"));
    }
    if let Some(pw) = &req.password {
        if pw.chars().count() < 6 || pw.len() > 256 {
            return Err(ApiError::bad("share passwords must be 6-256 characters"));
        }
    }
    let b = bucket_by_name(&state, &req.bucket).await?;
    let obj = state.engine.live_object(&b.id, &req.key).await?.ok_or_else(|| ApiError::not_found("object"))?;
    let token = random_token(32);
    let token_hash = state.keys.hash_secret(&token);
    let pw_hash = match req.password.clone() {
        Some(pw) => Some(tokio::task::spawn_blocking(move || hash_password(&pw)).await.map_err(|e| ApiError::Internal(e.to_string()))??),
        None => None,
    };
    let id = new_id();
    let expires_at = req.expires_in.map(|e| now() + e);
    let (sid, oid, actor) = (id.clone(), obj.id.clone(), p.actor.clone());
    state
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO shares (id, token_hash, object_id, password_hash, expires_at, max_downloads, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![sid, token_hash, oid, pw_hash, expires_at, req.max_downloads, actor, now()],
            )?;
            Ok(())
        })
        .await?;
    state.db.audit(&p.actor, "share.create", &format!("{}/{}", req.bucket, req.key), &p.ip.to_string(), Some(id.clone())).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "url": format!("{}/s/{token}", state.cfg.public_url),
            "raw_url": format!("{}/s/{token}/raw", state.cfg.public_url),
            "expires_at": expires_at,
            "max_downloads": req.max_downloads,
            "password": req.password.is_some(),
        })),
    ))
}

pub async fn list(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::Share)?;
    let (admin, actor) = (p.is_admin(), p.actor.clone());
    let items: Vec<Value> = state
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT s.id, b.name, o.key, s.password_hash IS NOT NULL, s.expires_at, s.max_downloads, s.downloads, s.created_by, s.created_at, s.revoked_at
                 FROM shares s JOIN objects o ON o.id = s.object_id JOIN buckets b ON b.id = o.bucket_id
                 WHERE (?1 OR s.created_by = ?2) ORDER BY s.created_at DESC LIMIT 500",
            )?;
            let v = st
                .query_map(params![admin, actor], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?, "bucket": r.get::<_, String>(1)?, "key": r.get::<_, String>(2)?,
                        "password": r.get::<_, bool>(3)?, "expires_at": r.get::<_, Option<i64>>(4)?,
                        "max_downloads": r.get::<_, Option<i64>>(5)?, "downloads": r.get::<_, i64>(6)?,
                        "created_by": r.get::<_, String>(7)?, "created_at": r.get::<_, i64>(8)?, "revoked_at": r.get::<_, Option<i64>>(9)?,
                    }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(json!({ "shares": items })))
}

pub async fn revoke(State(state): State<AppState>, p: Principal, Path(id): Path<String>) -> ApiResult<StatusCode> {
    p.require(Scope::Share)?;
    let (sid, admin, actor) = (id.clone(), p.is_admin(), p.actor.clone());
    let n = state
        .db
        .call(move |c| {
            Ok(c.execute(
                "UPDATE shares SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL AND (?3 OR created_by = ?4)",
                params![now(), sid, admin, actor],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(ApiError::not_found("active share"));
    }
    state.db.audit(&p.actor, "share.revoke", &id, &p.ip.to_string(), None).await;
    Ok(StatusCode::NO_CONTENT)
}

struct ShareRow {
    id: String,
    password_hash: Option<String>,
    expires_at: Option<i64>,
    max_downloads: Option<i64>,
    downloads: i64,
    obj: Object,
}

/// Resolve a token to a usable share. Every failure is the same 404, so a
/// probe cannot tell "revoked" from "expired" from "never existed".
async fn resolve(state: &AppState, token: &str) -> ApiResult<ShareRow> {
    if token.len() != 32 || !token.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(ApiError::not_found("share"));
    }
    let hash = state.keys.hash_secret(token);
    let row: Option<(String, String, Option<String>, Option<i64>, Option<i64>, i64, Option<i64>)> = state
        .db
        .call(move |c| {
            Ok(c.query_row(
                "SELECT id, object_id, password_hash, expires_at, max_downloads, downloads, revoked_at FROM shares WHERE token_hash = ?1",
                [hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()?)
        })
        .await?;
    let Some((id, oid, password_hash, expires_at, max_downloads, downloads, revoked)) = row else { return Err(ApiError::not_found("share")) };
    if revoked.is_some() || expires_at.is_some_and(|e| e <= now()) {
        return Err(ApiError::not_found("share"));
    }
    let obj = state.engine.object_by_id(&oid).await?.ok_or_else(|| ApiError::not_found("share"))?;
    if obj.deleted_at.is_some() || obj.expires_at.is_some_and(|e| e <= now()) {
        return Err(ApiError::not_found("share"));
    }
    Ok(ShareRow { id, password_hash, expires_at, max_downloads, downloads, obj })
}

fn ticket_cookie_name(share_id: &str) -> String {
    format!("tgv_share_{}", &share_id[..12])
}

fn ticket(state: &AppState, share_id: &str, exp: i64) -> String {
    format!("{exp}.{}", state.keys.share_sig(&[share_id.as_bytes(), exp.to_string().as_bytes()]))
}

fn ticket_ok(state: &AppState, share_id: &str, headers: &HeaderMap) -> bool {
    let Some(t) = named_cookie(headers, &ticket_cookie_name(share_id)) else { return false };
    let Some((exp, _)) = t.split_once('.') else { return false };
    let Ok(exp) = exp.parse::<i64>() else { return false };
    exp > now() && ct_eq(ticket(state, share_id, exp).as_bytes(), t.as_bytes())
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn human(n: i64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", units[i]) }
}

fn page(title: &str, body: &str) -> Response {
    let html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex"><title>{}</title><link rel="stylesheet" href="/assets/share.css"></head>
<body><main class="share">{body}</main></body></html>"#,
        esc(title)
    );
    let mut r = Html(html).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; style-src 'self'; media-src 'self'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'".parse().unwrap(),
    );
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}

pub async fn landing(State(state): State<AppState>, _c: PublicClient, Path(token): Path<String>, uri: Uri, headers: HeaderMap) -> Response {
    let share = match resolve(&state, &token).await {
        Ok(s) => s,
        Err(_) => {
            let mut r = page("Link unavailable", "<h1>This link is not available</h1><p>It may have expired, reached its download limit, or been turned off by its owner.</p>");
            *r.status_mut() = StatusCode::NOT_FOUND;
            return r;
        }
    };
    let name = share.obj.key.rsplit('/').next().unwrap_or(&share.obj.key).to_string();
    let locked = share.password_hash.is_some() && !ticket_ok(&state, &share.id, &headers);
    let wrong = uri.query().is_some_and(|q| q.contains("wrong=1"));
    let used_up = share.max_downloads.is_some_and(|m| share.downloads >= m);
    let raw = format!("/s/{}/raw", esc(&token));
    let ct = share.obj.content_type.as_str();
    let mut body = format!("<h1>{}</h1><p class=\"meta\">{}, {}</p>", esc(&name), human(share.obj.size), esc(ct));
    if used_up {
        body.push_str("<p>This link has reached its download limit.</p>");
    } else if locked {
        body.push_str(&format!(
            r#"<form method="post" action="/s/{}/unlock"><label for="pw">This file is protected. Enter the password you were given.</label>
<input id="pw" name="password" type="password" autocomplete="current-password" required autofocus>{}<button type="submit">Unlock</button></form>"#,
            esc(&token),
            if wrong { r#"<p class="err" role="alert">That password is not right. Check it and try again.</p>"# } else { "" }
        ));
    } else {
        if ct.starts_with("video/") {
            body.push_str(&format!(r#"<video src="{raw}" controls playsinline preload="metadata"></video>"#));
        } else if ct.starts_with("audio/") {
            body.push_str(&format!(r#"<audio src="{raw}" controls preload="metadata"></audio>"#));
        } else if ct.starts_with("image/") && ct != "image/svg+xml" {
            body.push_str(&format!(r#"<img src="{raw}" alt="{}">"#, esc(&name)));
        }
        body.push_str(&format!(r#"<p><a class="btn" href="{raw}?download=1">Download</a></p>"#));
        if let Some(e) = share.expires_at {
            body.push_str(&format!("<p class=\"meta\">Link expires {}</p>", super::serve::httpdate(e)));
        }
    }
    page(&name, &body)
}

#[derive(Deserialize)]
pub struct UnlockForm {
    pub password: String,
}

pub async fn unlock(State(state): State<AppState>, c: PublicClient, Path(token): Path<String>, Form(f): Form<UnlockForm>) -> ApiResult<Response> {
    let share = resolve(&state, &token).await?;
    let Some(phc) = share.password_hash.clone() else { return Ok(Redirect::to(&format!("/s/{token}")).into_response()) };
    // Per-share limit on top of the per-IP one: slows distributed guessing.
    state.limiter.check(&format!("unlock:{}", share.id), 20)?;
    if f.password.len() > 1024 {
        return Ok(Redirect::to(&format!("/s/{token}?wrong=1")).into_response());
    }
    let ok = verify_password_async(f.password, phc).await;
    if !ok {
        state.db.audit("public", "share.unlock_failed", &share.id, &c.0.to_string(), None).await;
        return Ok(Redirect::to(&format!("/s/{token}?wrong=1")).into_response());
    }
    let exp = now() + 3600;
    let mut r = Redirect::to(&format!("/s/{token}")).into_response();
    let secure = if state.cfg.cookie_secure { "; Secure" } else { "" };
    let cookie = format!(
        "{}={}; Path=/s/{token}; Max-Age=3600; HttpOnly; SameSite=Lax{secure}",
        ticket_cookie_name(&share.id),
        ticket(&state, &share.id, exp)
    );
    r.headers_mut().insert(header::SET_COOKIE, cookie.parse().map_err(|_| ApiError::Internal("cookie".into()))?);
    Ok(r)
}

#[derive(Deserialize, Default)]
pub struct RawQuery {
    #[serde(default)]
    pub download: bool,
}

pub async fn raw(State(state): State<AppState>, _c: PublicClient, method: Method, Path(token): Path<String>, uri: Uri, headers: HeaderMap) -> ApiResult<Response> {
    let headers = &headers;
    let q: RawQuery = uri.query().and_then(serde_urlencoded_lite).unwrap_or_default();
    let share = resolve(&state, &token).await?;
    if share.password_hash.is_some() && !ticket_ok(&state, &share.id, headers) {
        return Err(ApiError::Forbidden("unlock this share with its password first".into()));
    }
    // Every GET without a download ticket counts (check-and-increment is a single
    // statement, so parallel requests cannot overshoot the cap). The counted
    // response hands out a 6-hour ticket cookie, so the same player can seek with
    // Range requests for free. Counting only "starts at byte 0" would let anyone
    // bypass max_downloads with `Range: bytes=1-`.
    let mut ticket_cookie = None;
    if method == Method::GET && !dl_ticket_ok(&state, &share.id, headers) {
        let sid = share.id.clone();
        let n = state
            .db
            .call(move |c| {
                Ok(c.execute(
                    "UPDATE shares SET downloads = downloads + 1 WHERE id = ?1 AND (max_downloads IS NULL OR downloads < max_downloads)",
                    [sid],
                )?)
            })
            .await?;
        if n == 0 {
            return Err(ApiError::not_found("share"));
        }
        let exp = now() + 6 * 3600;
        let secure = if state.cfg.cookie_secure { "; Secure" } else { "" };
        ticket_cookie = Some(format!(
            "{}={}; Path=/s/{token}/raw; Max-Age=21600; HttpOnly; SameSite=Lax{secure}",
            dl_cookie_name(&share.id),
            dl_ticket(&state, &share.id, exp)
        ));
    } else if share.max_downloads.is_some_and(|m| share.downloads > m) {
        return Err(ApiError::not_found("share"));
    }
    let mut res = serve_object(&state, &share.obj, &method, headers, ServeOpts { force_download: q.download, cache_control: "private, no-store" }).await?;
    if let Some(c) = ticket_cookie.and_then(|c| c.parse().ok()) {
        res.headers_mut().insert(header::SET_COOKIE, c);
    }
    Ok(res)
}

fn dl_cookie_name(share_id: &str) -> String {
    format!("tgv_dl_{}", &share_id[..12])
}

fn dl_ticket(state: &AppState, share_id: &str, exp: i64) -> String {
    format!("{exp}.{}", state.keys.share_sig(&[b"download", share_id.as_bytes(), exp.to_string().as_bytes()]))
}

fn dl_ticket_ok(state: &AppState, share_id: &str, headers: &HeaderMap) -> bool {
    let Some(t) = named_cookie(headers, &dl_cookie_name(share_id)) else { return false };
    let Some((exp, _)) = t.split_once('.') else { return false };
    let Ok(exp) = exp.parse::<i64>() else { return false };
    exp > now() && ct_eq(dl_ticket(state, share_id, exp).as_bytes(), t.as_bytes())
}

fn serde_urlencoded_lite(q: &str) -> Option<RawQuery> {
    Some(RawQuery { download: q.split('&').any(|kv| kv == "download=1" || kv == "download=true") })
}
