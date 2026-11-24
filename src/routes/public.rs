//! Unauthenticated access paths, R2-style:
//! * `/pub/{bucket}/{key}`  — public buckets (like an r2.dev URL)
//! * `/p/{bucket}/{key}?…`  — presigned GET/PUT URLs with an expiry, bound to the issuing key
//!
//! Both answer CORS preflights using the bucket's own origin allowlist, so other
//! websites can embed `<video src>` or upload straight from the browser.

use super::serve::{serve_object, ServeOpts};
use super::{bucket_by_name, guess_type, validate_key};
use crate::auth::{PublicClient, Principal, Scope};
use crate::crypto::ct_eq;
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::storage::{Bucket, NewObject};
use crate::AppState;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_PRESIGN_SECS: i64 = 7 * 24 * 3600;
const PATH_SEGMENT: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'#').add(b'<').add(b'>').add(b'?').add(b'`').add(b'{').add(b'}').add(b'%').add(b'\\').add(b'^').add(b'|').add(b'[').add(b']');

pub fn cors(bucket: &Bucket, headers: &HeaderMap, res: &mut Response) {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else { return };
    let h = res.headers_mut();
    h.append(header::VARY, HeaderValue::from_static("Origin"));
    let allowed = bucket.cors_origins.iter().any(|o| o == "*" || o == origin);
    if !allowed {
        return;
    }
    let value = if bucket.cors_origins.iter().any(|o| o == origin) { origin } else { "*" };
    if let Ok(v) = HeaderValue::from_str(value) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        h.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static("content-length, content-range, accept-ranges, etag, x-tgv-object-id"));
    }
}

pub async fn preflight(State(state): State<AppState>, _c: PublicClient, Path((bucket, _key)): Path<(String, String)>, headers: HeaderMap) -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    if let Ok(b) = bucket_by_name(&state, &bucket).await {
        cors(&b, &headers, &mut res);
        if res.headers().contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN) {
            let h = res.headers_mut();
            h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, HEAD, PUT"));
            h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("range, content-type, if-none-match, if-range"));
            h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        }
    }
    res
}

#[derive(Deserialize, Default)]
pub struct PubQuery {
    #[serde(default)]
    pub download: bool,
}

pub async fn public_bucket(
    State(state): State<AppState>,
    _c: PublicClient,
    method: Method,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<PubQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let b = bucket_by_name(&state, &bucket).await?;
    if !b.public {
        // Private buckets are indistinguishable from missing ones here.
        return Err(ApiError::not_found("object"));
    }
    let obj = state.engine.live_object(&b.id, &key).await?.ok_or_else(|| ApiError::not_found("object"))?;
    let mut res = serve_object(&state, &obj, &method, &headers, ServeOpts { force_download: q.download, cache_control: "public, max-age=300" }).await?;
    cors(&b, &headers, &mut res);
    Ok(res)
}

#[derive(Deserialize)]
pub struct PresignReq {
    pub bucket: String,
    pub key: String,
    #[serde(default = "default_method")]
    pub method: String,
    pub expires_in: Option<i64>,
    /// PUT only: the upload must send exactly this Content-Type.
    pub content_type: Option<String>,
}

fn default_method() -> String {
    "GET".into()
}

fn sign(state: &AppState, method: &str, bucket: &str, key: &str, exp: i64, kid: &str, ct: &str) -> String {
    state.keys.presign_sig(&[method.as_bytes(), bucket.as_bytes(), key.as_bytes(), exp.to_string().as_bytes(), kid.as_bytes(), ct.as_bytes()])
}

pub async fn presign(State(state): State<AppState>, p: Principal, Json(req): Json<PresignReq>) -> ApiResult<Json<Value>> {
    let method = req.method.to_ascii_uppercase();
    let scope = match method.as_str() {
        "GET" => Scope::Read,
        "PUT" => Scope::Write,
        _ => return Err(ApiError::bad("method must be GET or PUT")),
    };
    p.require_bucket(scope, &req.bucket)?;
    validate_key(&req.key)?;
    let b = bucket_by_name(&state, &req.bucket).await?;
    if method == "GET" && state.engine.live_object(&b.id, &req.key).await?.is_none() {
        return Err(ApiError::not_found("object"));
    }
    let ttl = req.expires_in.unwrap_or(3600);
    if !(1..=MAX_PRESIGN_SECS).contains(&ttl) {
        return Err(ApiError::bad(format!("expires_in must be 1-{MAX_PRESIGN_SECS} seconds")));
    }
    let ct = if method == "PUT" { req.content_type.as_deref().map(|c| guess_type(&req.key, Some(c))).unwrap_or_default() } else { String::new() };
    let exp = now() + ttl;
    let sig = sign(&state, &method, &req.bucket, &req.key, exp, &p.actor, &ct);
    let path: Vec<String> = req.key.split('/').map(|s| utf8_percent_encode(s, PATH_SEGMENT).to_string()).collect();
    let mut url = format!("{}/p/{}/{}?m={method}&exp={exp}&kid={}&sig={sig}", state.cfg.public_url, req.bucket, path.join("/"), p.actor);
    if !ct.is_empty() {
        url.push_str(&format!("&ct={}", utf8_percent_encode(&ct, percent_encoding::NON_ALPHANUMERIC)));
    }
    state.db.audit(&p.actor, "presign", &format!("{method} {}/{}", req.bucket, req.key), &p.ip.to_string(), Some(format!("ttl={ttl}"))).await;
    Ok(Json(json!({ "url": url, "method": method, "expires_at": exp, "content_type": if ct.is_empty() { None } else { Some(ct) } })))
}

#[derive(Deserialize)]
pub struct PresignedQuery {
    pub m: String,
    pub exp: i64,
    pub kid: String,
    pub sig: String,
    #[serde(default)]
    pub ct: String,
    #[serde(default)]
    pub download: bool,
}

/// The URL is only as good as the key that signed it: a revoked, expired or
/// down-scoped key invalidates every URL it ever issued.
async fn verify(state: &AppState, method: &str, bucket: &str, key: &str, q: &PresignedQuery) -> ApiResult<()> {
    let denied = || ApiError::Forbidden("this link is invalid or has expired".into());
    if q.m != method || q.exp <= now() || q.exp > now() + MAX_PRESIGN_SECS {
        return Err(denied());
    }
    let expected = sign(state, method, bucket, key, q.exp, &q.kid, &q.ct);
    if !ct_eq(expected.as_bytes(), q.sig.as_bytes()) {
        return Err(denied());
    }
    if q.kid == "admin" {
        return Ok(());
    }
    let kid = q.kid.clone();
    let row: Option<(String, Option<String>, Option<i64>, Option<i64>)> = state
        .db
        .call(move |c| {
            Ok(c.query_row("SELECT scopes, buckets, expires_at, revoked_at FROM api_keys WHERE id = ?1", [kid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?)
        })
        .await?;
    let Some((scopes, buckets, exp, revoked)) = row else { return Err(denied()) };
    if revoked.is_some() || exp.is_some_and(|e| e <= now()) {
        return Err(denied());
    }
    let p = Principal {
        actor: q.kid.clone(),
        scopes: serde_json::from_str(&scopes).unwrap_or_default(),
        buckets: buckets.and_then(|b| serde_json::from_str(&b).ok()),
        ip: std::net::IpAddr::from([0, 0, 0, 0]),
        via_session: false,
    };
    let scope = if method == "GET" { Scope::Read } else { Scope::Write };
    p.require_bucket(scope, bucket).map_err(|_| denied())
}

pub async fn presigned_get(
    State(state): State<AppState>,
    _c: PublicClient,
    method: Method,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<PresignedQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    verify(&state, "GET", &bucket, &key, &q).await?;
    let b = bucket_by_name(&state, &bucket).await?;
    let obj = state.engine.live_object(&b.id, &key).await?.ok_or_else(|| ApiError::not_found("object"))?;
    let mut res = serve_object(&state, &obj, &method, &headers, ServeOpts { force_download: q.download, cache_control: "private, max-age=60" }).await?;
    cors(&b, &headers, &mut res);
    Ok(res)
}

pub async fn presigned_put(
    State(state): State<AppState>,
    c: PublicClient,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<PresignedQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Response> {
    verify(&state, "PUT", &bucket, &key, &q).await?;
    validate_key(&key)?;
    let sent_ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !q.ct.is_empty() && guess_type(&key, Some(sent_ct)) != q.ct {
        return Err(ApiError::Forbidden(format!("this link only accepts Content-Type {}", q.ct)));
    }
    let b = bucket_by_name(&state, &bucket).await?;
    let left = super::objects::quota_left(&state, &b).await?;
    let obj = NewObject {
        bucket_id: b.id.clone(),
        key: key.clone(),
        content_type: guess_type(&key, Some(sent_ct)),
        meta: serde_json::json!({}),
        expires_at: super::objects::ttl_from(&headers, &b)?,
    };
    let created = state.engine.put_object(obj, body.into_data_stream(), left).await?;
    state.db.audit(&q.kid, "object.put_presigned", &format!("{bucket}/{key}"), &c.0.to_string(), Some(format!("{} bytes", created.size))).await;
    let mut res = (StatusCode::CREATED, Json(created)).into_response();
    cors(&b, &headers, &mut res);
    Ok(res)
}
