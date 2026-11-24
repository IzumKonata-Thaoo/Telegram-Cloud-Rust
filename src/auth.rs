//! Authentication: scoped API keys, the admin panel session, and rate limiting.
//!
//! API key format: `tgv_<id>_<secret>`. Only `HMAC(pepper, secret)` is stored, the
//! comparison is constant-time, and a key can be limited by scope, bucket, IP,
//! expiry and request rate (modelled after R2 API tokens).

use crate::crypto::{ct_eq, random_token};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use dashmap::DashMap;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    Write,
    Delete,
    List,
    Share,
    Admin,
}

impl Scope {
    pub const ALL: [Scope; 6] = [Scope::Read, Scope::Write, Scope::Delete, Scope::List, Scope::Share, Scope::Admin];
}

#[derive(Debug, Clone)]
pub struct Principal {
    /// "admin" for the panel session, else the API key id.
    pub actor: String,
    pub scopes: Vec<Scope>,
    /// None = every bucket.
    pub buckets: Option<Vec<String>>,
    pub ip: IpAddr,
    pub via_session: bool,
}

impl Principal {
    pub fn is_admin(&self) -> bool {
        self.scopes.contains(&Scope::Admin)
    }

    pub fn require(&self, scope: Scope) -> ApiResult<()> {
        if self.is_admin() || self.scopes.contains(&scope) {
            Ok(())
        } else {
            Err(ApiError::Forbidden(format!("this key lacks the '{}' scope", serde_json::to_value(scope).unwrap().as_str().unwrap_or("?"))))
        }
    }

    pub fn require_bucket(&self, scope: Scope, bucket: &str) -> ApiResult<()> {
        self.require(scope)?;
        match &self.buckets {
            Some(list) if !self.is_admin() && !list.iter().any(|b| b == bucket) => {
                Err(ApiError::Forbidden(format!("this key is not allowed to access bucket '{bucket}'")))
            }
            _ => Ok(()),
        }
    }

    pub fn can_see_bucket(&self, bucket: &str) -> bool {
        self.is_admin() || self.buckets.as_ref().is_none_or(|l| l.iter().any(|b| b == bucket))
    }
}

pub struct Session {
    pub csrf: String,
    pub expires_at: i64,
}

/// Fixed-window-free token bucket: `rpm` tokens per minute, burst of `rpm / 4` (min 10).
pub struct RateLimiter {
    buckets: DashMap<String, (f64, Instant)>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self { buckets: DashMap::new() }
    }

    pub fn check(&self, key: &str, rpm: u32) -> ApiResult<()> {
        if rpm == 0 {
            return Ok(());
        }
        let cap = (rpm as f64 / 4.0).max(10.0);
        let rate = rpm as f64 / 60.0;
        let mut e = self.buckets.entry(key.to_string()).or_insert((cap, Instant::now()));
        let (tokens, last) = &mut *e;
        let elapsed = last.elapsed().as_secs_f64();
        *tokens = (*tokens + elapsed * rate).min(cap);
        *last = Instant::now();
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(ApiError::RateLimited { retry_after: ((1.0 - *tokens) / rate).ceil().max(1.0) as u64 })
        }
    }

    /// True when `key` has no token left, without consuming one.
    pub fn exhausted(&self, key: &str, rpm: u32) -> bool {
        let Some(e) = self.buckets.get(key) else { return false };
        let cap = (rpm as f64 / 4.0).max(10.0);
        let (tokens, last) = *e;
        (tokens + last.elapsed().as_secs_f64() * rpm as f64 / 60.0).min(cap) < 1.0
    }

    pub fn sweep(&self) {
        self.buckets.retain(|_, (_, last)| last.elapsed().as_secs() < 600);
    }
}

pub fn client_ip(parts: &Parts, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        // Use the *last* hop: it was appended by the proxy we trust. Earlier entries
        // are whatever the client sent and would let anyone pick their own IP,
        // bypassing per-IP rate limits and key IP allowlists.
        if let Some(ip) = parts
            .headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .last()
            .and_then(|v| v.trim().parse().ok())
        {
            return ip;
        }
    }
    parts
        .extensions
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::from([0, 0, 0, 0]))
}

/// `ip` matches an entry that is either an address or a CIDR block.
pub fn ip_allowed(ip: IpAddr, list: &[String]) -> bool {
    list.iter().any(|entry| {
        let (net, bits) = match entry.split_once('/') {
            Some((n, b)) => (n, b.parse::<u32>().ok()),
            None => (entry.as_str(), None),
        };
        let Ok(net) = net.parse::<IpAddr>() else { return false };
        match (ip, net) {
            (IpAddr::V4(a), IpAddr::V4(n)) => {
                let bits = bits.unwrap_or(32).min(32);
                let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
                (u32::from(a) & mask) == (u32::from(n) & mask)
            }
            (IpAddr::V6(a), IpAddr::V6(n)) => {
                let bits = bits.unwrap_or(128).min(128);
                let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
                (u128::from(a) & mask) == (u128::from(n) & mask)
            }
            _ => false,
        }
    })
}

pub fn parse_api_key(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("tgv_")?;
    let (id, secret) = rest.split_once('_')?;
    let valid = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_alphanumeric());
    (valid(id, 12) && valid(secret, 40)).then_some((id, secret))
}

pub fn new_api_key() -> (String, String, String) {
    let id = random_token(12);
    let secret = random_token(40);
    let full = format!("tgv_{id}_{secret}");
    (id, secret, full)
}

fn cookie<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all("cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

pub fn session_cookie(headers: &axum::http::HeaderMap) -> Option<&str> {
    cookie(headers, "tgv_session")
}

pub fn named_cookie<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    cookie(headers, name)
}

async fn from_api_key(state: &AppState, raw: &str, ip: IpAddr) -> ApiResult<Principal> {
    let (id, secret) = parse_api_key(raw).ok_or(ApiError::Unauthorized)?;
    let hash = state.keys.hash_secret(secret);
    let id_owned = id.to_string();
    type Row = (String, String, Option<String>, Option<String>, Option<i64>, Option<i64>, Option<i64>);
    let row: Option<Row> = state
        .db
        .call(move |c| {
            Ok(c.query_row(
                "SELECT secret_hash, scopes, buckets, allowed_ips, rpm, expires_at, revoked_at FROM api_keys WHERE id = ?1",
                [&id_owned],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()?)
        })
        .await?;
    // Same error for "no such key" and "wrong secret": no key-id oracle.
    let Some((stored, scopes, buckets, ips, rpm, expires_at, revoked_at)) = row else { return Err(ApiError::Unauthorized) };
    if !ct_eq(stored.as_bytes(), hash.as_bytes()) || revoked_at.is_some() || expires_at.is_some_and(|e| e <= now()) {
        return Err(ApiError::Unauthorized);
    }
    if let Some(ips) = ips {
        let list: Vec<String> = serde_json::from_str(&ips).unwrap_or_default();
        if !ip_allowed(ip, &list) {
            return Err(ApiError::Forbidden("this key cannot be used from your IP address".into()));
        }
    }
    let rpm = rpm.map(|r| r.max(0) as u32).unwrap_or(state.cfg.default_key_rpm);
    state.limiter.check(&format!("key:{id}"), rpm)?;
    let idc = id.to_string();
    let db = state.db.clone();
    // Fire-and-forget; last_used_at is informational.
    tokio::spawn(async move {
        let _ = db
            .call(move |c| {
                c.execute("UPDATE api_keys SET last_used_at = ?1 WHERE id = ?2", rusqlite::params![now(), idc])?;
                Ok(())
            })
            .await;
    });
    Ok(Principal {
        actor: id.to_string(),
        scopes: serde_json::from_str(&scopes).unwrap_or_default(),
        buckets: buckets.and_then(|b| serde_json::from_str(&b).ok()),
        ip,
        via_session: false,
    })
}

fn from_session(state: &AppState, parts: &Parts, ip: IpAddr) -> ApiResult<Option<Principal>> {
    let Some(sid) = session_cookie(&parts.headers) else { return Ok(None) };
    let key = state.keys.hash_secret(sid);
    let Some(sess) = state.sessions.get(&key) else { return Ok(None) };
    if sess.expires_at <= now() {
        drop(sess);
        state.sessions.remove(&key);
        return Ok(None);
    }
    // Cookie auth + state change => require the CSRF header (double submit). Safe
    // methods are exempt so <video src> and downloads keep working in the panel.
    let safe = matches!(parts.method.as_str(), "GET" | "HEAD" | "OPTIONS");
    if !safe {
        let sent = parts.headers.get("x-csrf-token").and_then(|v| v.to_str().ok()).unwrap_or("");
        if !ct_eq(sent.as_bytes(), sess.csrf.as_bytes()) {
            return Err(ApiError::Forbidden("missing or invalid CSRF token".into()));
        }
    }
    Ok(Some(Principal { actor: "admin".into(), scopes: Scope::ALL.to_vec(), buckets: None, ip, via_session: true }))
}

impl FromRequestParts<Arc<crate::AppStateInner>> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let ip = client_ip(parts, state.cfg.trust_proxy);
        if let Some(h) = parts.headers.get("authorization") {
            let v = h.to_str().map_err(|_| ApiError::Unauthorized)?;
            let token = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")).ok_or(ApiError::Unauthorized)?;
            // Throttle clients that keep presenting bad keys (60 failures/min per IP).
            // A valid key is always honoured: refusing it would let anyone behind the
            // same NAT lock real users out just by sending garbage keys.
            let fail_key = format!("authfail:{ip}");
            let r = from_api_key(state, token.trim(), ip).await;
            if matches!(r, Err(ApiError::Unauthorized)) && state.limiter.check(&fail_key, 60).is_err() {
                return Err(ApiError::RateLimited { retry_after: 30 });
            }
            return r;
        }
        if let Some(p) = from_session(state, parts, ip)? {
            return Ok(p);
        }
        Err(ApiError::Unauthorized)
    }
}

/// Client IP for public (unauthenticated) routes, with per-IP rate limiting.
pub struct PublicClient(pub IpAddr);

impl FromRequestParts<Arc<crate::AppStateInner>> for PublicClient {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let ip = client_ip(parts, state.cfg.trust_proxy);
        state.limiter.check(&format!("ip:{ip}"), state.cfg.public_rpm)?;
        Ok(Self(ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr() {
        let ip: IpAddr = "10.1.2.3".parse().unwrap();
        assert!(ip_allowed(ip, &["10.0.0.0/8".into()]));
        assert!(!ip_allowed(ip, &["10.1.3.0/24".into()]));
        assert!(ip_allowed(ip, &["10.1.2.3".into()]));
        assert!(!ip_allowed(ip, &["::1".into(), "garbage".into()]));
        assert!(ip_allowed(ip, &["0.0.0.0/0".into()]));
    }

    #[test]
    fn key_format() {
        let (id, secret, full) = new_api_key();
        assert_eq!(parse_api_key(&full), Some((id.as_str(), secret.as_str())));
        assert!(parse_api_key("tgv_short_x").is_none());
        assert!(parse_api_key(&full.replace("tgv_", "xx_")).is_none());
    }

    #[test]
    fn xff_uses_last_hop() {
        let req = axum::http::Request::builder()
            .header("x-forwarded-for", "1.2.3.4, 10.0.0.9")
            .body(())
            .unwrap();
        let (parts, _) = req.into_parts();
        assert_eq!(client_ip(&parts, true), "10.0.0.9".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn limiter() {
        let l = RateLimiter::new();
        for _ in 0..10 {
            l.check("k", 10).unwrap();
        }
        assert!(l.check("k", 10).is_err());
    }
}
