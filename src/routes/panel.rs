//! Admin panel: login session + static assets (embedded in the binary).

use crate::auth::{session_cookie, Principal, PublicClient, Session};
use crate::crypto::random_token;
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

const PANEL_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; \
img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'; object-src 'none'";

struct Asset {
    body: &'static str,
    ctype: &'static str,
}

fn asset_for(name: &str) -> Option<Asset> {
    Some(match name {
        "app.js" => Asset { body: include_str!("../../web/app.js"), ctype: "text/javascript; charset=utf-8" },
        "app.css" => Asset { body: include_str!("../../web/app.css"), ctype: "text/css; charset=utf-8" },
        "docs.js" => Asset { body: include_str!("../../web/docs.js"), ctype: "text/javascript; charset=utf-8" },
        "share.css" => Asset { body: include_str!("../../web/share.css"), ctype: "text/css; charset=utf-8" },
        _ => return None,
    })
}

fn html(body: &'static str) -> Response {
    let mut r = (StatusCode::OK, body).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(PANEL_CSP));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

pub async fn index() -> Response {
    html(include_str!("../../web/index.html"))
}

pub async fn docs() -> Response {
    html(include_str!("../../web/docs.html"))
}

pub async fn asset(Path(file): Path<String>) -> Response {
    match asset_for(&file) {
        Some(a) => {
            let mut r = a.body.into_response();
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(a.ctype));
            // Assets are compiled into the binary; tiny, so never let a stale copy outlive an upgrade.
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        None => ApiError::not_found("asset").into_response(),
    }
}

pub async fn openapi(State(state): State<AppState>) -> Response {
    let mut spec: Value = serde_json::from_str(include_str!("../../web/openapi.json")).unwrap_or(json!({}));
    spec["servers"] = json!([{ "url": state.cfg.public_url }]);
    Json(spec).into_response()
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub password: String,
}

pub async fn login(State(state): State<AppState>, c: PublicClient, Json(req): Json<LoginReq>) -> ApiResult<Response> {
    let Some(phc) = state.admin_hash.clone() else {
        return Err(ApiError::Forbidden("the panel is disabled: set TGV_ADMIN_PASSWORD or TGV_ADMIN_PASSWORD_HASH".into()));
    };
    state.limiter.check(&format!("login:{}", c.0), 10)?;
    if req.password.len() > 1024 {
        return Err(ApiError::Unauthorized);
    }
    let ok = super::shares::verify_password_async(req.password, phc).await;
    if !ok {
        state.db.audit("public", "login.failed", "panel", &c.0.to_string(), None).await;
        return Err(ApiError::Unauthorized);
    }
    let sid = random_token(48);
    let csrf = random_token(32);
    let expires_at = now() + state.cfg.session_hours * 3600;
    state.sessions.insert(state.keys.hash_secret(&sid), Session { csrf: csrf.clone(), expires_at });
    state.db.audit("admin", "login", "panel", &c.0.to_string(), None).await;
    let secure = if state.cfg.cookie_secure { "; Secure" } else { "" };
    let cookie = format!("tgv_session={sid}; Path=/; Max-Age={}; HttpOnly; SameSite=Strict{secure}", state.cfg.session_hours * 3600);
    let mut r = Json(json!({ "csrf": csrf, "expires_at": expires_at })).into_response();
    r.headers_mut().insert(header::SET_COOKIE, cookie.parse().map_err(|_| ApiError::Internal("cookie".into()))?);
    Ok(r)
}

pub async fn logout(State(state): State<AppState>, p: Principal, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(sid) = session_cookie(&headers) {
        state.sessions.remove(&state.keys.hash_secret(sid));
    }
    let _ = p;
    let mut r = StatusCode::NO_CONTENT.into_response();
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("tgv_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict"));
    Ok(r)
}

pub async fn me(State(state): State<AppState>, p: Principal, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let csrf = if p.via_session {
        session_cookie(&headers).and_then(|sid| state.sessions.get(&state.keys.hash_secret(sid)).map(|s| s.csrf.clone()))
    } else {
        None
    };
    Ok(Json(json!({ "actor": p.actor, "scopes": p.scopes, "buckets": p.buckets, "csrf": csrf, "public_url": state.cfg.public_url })))
}
