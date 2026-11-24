//! Remote import: the server downloads a URL and streams it straight into the
//! vault (encrypted chunks -> Telegram). Nothing is buffered to disk.

use super::objects::{quota_left, ttl_from};
use super::{bucket_by_name, guess_type, validate_key};
use crate::auth::{Principal, Scope};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::storage::{new_id, NewObject};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct Job {
    pub id: String,
    pub owner: String,
    pub bucket: String,
    pub key: String,
    /// scheme://host/path only; the query string may carry tokens.
    pub source: String,
    pub created_at: i64,
    pub state: Mutex<&'static str>,
    pub received: AtomicU64,
    pub total: AtomicU64,
    pub error: Mutex<Option<String>>,
    pub object: Mutex<Option<Value>>,
    pub finished_at: AtomicU64,
}

impl Job {
    pub fn view(&self) -> Value {
        let total = self.total.load(Ordering::Relaxed);
        json!({
            "id": self.id,
            "bucket": self.bucket,
            "key": self.key,
            "source": self.source,
            "state": *self.state.lock(),
            "received_bytes": self.received.load(Ordering::Relaxed),
            "total_bytes": if total == 0 { Value::Null } else { json!(total) },
            "error": self.error.lock().clone(),
            "object": self.object.lock().clone(),
            "created_at": self.created_at,
            "finished_at": match self.finished_at.load(Ordering::Relaxed) { 0 => Value::Null, t => json!(t) },
        })
    }
}

#[derive(Deserialize)]
pub struct ImportReq {
    pub url: String,
    /// Object key. Defaults to the last path segment of the URL.
    pub key: Option<String>,
    pub content_type: Option<String>,
    /// Extra request headers for the source server (Referer, Authorization, Cookie…).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Block until the import finishes and return the object (handy in scripts).
    #[serde(default)]
    pub wait: bool,
}

const FORBIDDEN_HEADERS: [&str; 8] = ["host", "content-length", "transfer-encoding", "connection", "upgrade", "te", "trailer", "proxy-authorization"];

fn default_key(url: &reqwest::Url) -> String {
    let seg = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let name = percent_encoding::percent_decode_str(seg).decode_utf8_lossy().to_string();
    let name: String = name.chars().filter(|c| !c.is_control() && *c != '/' && *c != '\\').collect();
    if name.is_empty() || name == "." || name == ".." { format!("import-{}", &new_id()[..8]) } else { name }
}

pub async fn start(
    State(state): State<AppState>,
    p: Principal,
    Path(bucket): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ImportReq>,
) -> ApiResult<Response> {
    p.require_bucket(Scope::Write, &bucket)?;
    let url: reqwest::Url = req.url.trim().parse().map_err(|_| ApiError::bad("url is not a valid absolute URL"))?;
    crate::fetch::check_url(&url).map_err(|e| ApiError::bad(e.to_string()))?;
    if req.headers.len() > 16 {
        return Err(ApiError::bad("at most 16 extra headers"));
    }
    let mut extra = reqwest::header::HeaderMap::new();
    for (k, v) in &req.headers {
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|_| ApiError::bad(format!("invalid header name '{k}'")))?;
        if FORBIDDEN_HEADERS.contains(&name.as_str()) {
            return Err(ApiError::bad(format!("header '{k}' cannot be set")));
        }
        let value = reqwest::header::HeaderValue::from_str(v).map_err(|_| ApiError::bad(format!("invalid value for header '{k}'")))?;
        extra.insert(name, value);
    }
    let key = req.key.clone().unwrap_or_else(|| default_key(&url));
    validate_key(&key)?;
    let b = bucket_by_name(&state, &bucket).await?;
    let expires_at = ttl_from(&headers, &b)?;

    let mut source = url.clone();
    source.set_query(None);
    source.set_fragment(None);
    let job = Arc::new(Job {
        id: new_id(),
        owner: p.actor.clone(),
        bucket: bucket.clone(),
        key: key.clone(),
        source: source.to_string(),
        created_at: now(),
        state: Mutex::new("queued"),
        received: AtomicU64::new(0),
        total: AtomicU64::new(0),
        error: Mutex::new(None),
        object: Mutex::new(None),
        finished_at: AtomicU64::new(0),
    });
    state.imports.insert(job.id.clone(), job.clone());
    state.db.audit(&p.actor, "import.start", &format!("{bucket}/{key}"), &p.ip.to_string(), Some(job.source.clone())).await;

    let (st, j) = (state.clone(), job.clone());
    let content_type = req.content_type.clone();
    let handle = tokio::spawn(async move {
        let r = run(&st, &j, url, extra, b, content_type, expires_at).await;
        match r {
            Ok(obj) => {
                *j.object.lock() = Some(obj);
                *j.state.lock() = "done";
            }
            Err(e) => {
                *j.error.lock() = Some(e.to_string());
                *j.state.lock() = "failed";
            }
        }
        j.finished_at.store(now() as u64, Ordering::Relaxed);
        let err = j.error.lock().clone(); // never hold a lock guard across .await
        let action = if err.is_some() { "import.failed" } else { "import.done" };
        st.db.audit(&j.owner, action, &format!("{}/{}", j.bucket, j.key), "server", err).await;
    });

    if req.wait {
        let _ = handle.await;
        let v = job.view();
        return match v["state"].as_str() {
            Some("done") => Ok((StatusCode::CREATED, Json(v["object"].clone())).into_response()),
            _ => Err(ApiError::BadRequest(format!("import failed: {}", v["error"].as_str().unwrap_or("unknown error")))),
        };
    }
    Ok((StatusCode::ACCEPTED, Json(job.view())).into_response())
}

async fn run(
    state: &AppState,
    job: &Arc<Job>,
    url: reqwest::Url,
    extra: reqwest::header::HeaderMap,
    bucket: crate::storage::Bucket,
    content_type: Option<String>,
    expires_at: Option<i64>,
) -> ApiResult<Value> {
    let _slot = state.import_slots.acquire().await.map_err(|_| ApiError::Internal("import pool closed".into()))?;
    *job.state.lock() = "downloading";
    let resp = state
        .fetcher
        .get(url)
        .headers(extra)
        .send()
        .await
        .map_err(|e| ApiError::BadRequest(format!("could not reach the source: {}", root_cause(&e))))?;
    if !resp.status().is_success() {
        return Err(ApiError::BadRequest(format!("source answered HTTP {}", resp.status().as_u16())));
    }
    let left = quota_left(state, &bucket).await?;
    let limit = left.map_or(state.engine.max_upload, |l| l.min(state.engine.max_upload));
    if let Some(len) = resp.content_length() {
        if len > limit {
            return Err(ApiError::PayloadTooLarge(format!("source file is {len} bytes; the limit here is {limit}")));
        }
        job.total.store(len, Ordering::Relaxed);
    }
    let src_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let ct = guess_type(&job.key, content_type.as_deref().or(src_type.as_deref()));
    let counter = job.clone();
    let stream = Box::pin(resp.bytes_stream().map(move |r| {
        if let Ok(b) = &r {
            counter.received.fetch_add(b.len() as u64, Ordering::Relaxed);
        }
        r.map_err(|e| e.without_url())
    }));
    *job.state.lock() = "storing";
    let obj = state
        .engine
        .put_object(
            NewObject { bucket_id: bucket.id.clone(), key: job.key.clone(), content_type: ct, meta: json!({ "source": job.source }), expires_at },
            stream,
            Some(limit),
        )
        .await?;
    Ok(serde_json::to_value(obj).unwrap_or(Value::Null))
}

fn root_cause(e: &reqwest::Error) -> String {
    use std::error::Error;
    let mut msg = e.without_url_ref();
    let mut src = e.source();
    while let Some(s) = src {
        msg = s.to_string();
        src = s.source();
    }
    msg
}

trait WithoutUrlRef {
    fn without_url_ref(&self) -> String;
}
impl WithoutUrlRef for reqwest::Error {
    fn without_url_ref(&self) -> String {
        if self.is_timeout() { "timed out".into() } else if self.is_connect() { "connection failed".into() } else if self.is_redirect() { "redirect refused".into() } else { "request failed".into() }
    }
}

pub async fn status(State(state): State<AppState>, p: Principal, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let job = state.imports.get(&id).map(|j| j.clone()).ok_or_else(|| ApiError::not_found("import"))?;
    if !(p.is_admin() || job.owner == p.actor) {
        return Err(ApiError::not_found("import"));
    }
    Ok(Json(job.view()))
}

pub async fn list(State(state): State<AppState>, p: Principal) -> ApiResult<Json<Value>> {
    p.require(Scope::Write)?;
    let mut items: Vec<(i64, Value)> = state
        .imports
        .iter()
        .filter(|j| p.is_admin() || j.owner == p.actor)
        .map(|j| (j.created_at, j.view()))
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(Json(json!({ "imports": items.into_iter().map(|(_, v)| v).take(200).collect::<Vec<_>>() })))
}
