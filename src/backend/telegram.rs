//! Telegram Bot API backend.
//!
//! Chunks are sent as documents to one private channel. Several bots can share the
//! channel: uploads rotate across them, a bot that hits HTTP 429 cools down for the
//! `retry_after` Telegram asks for, and each chunk records which bot uploaded it,
//! because a `file_id` is only valid for the bot that received it.
//!
//! Size limits of the public Bot API: 50 MB upload, 20 MB download through
//! `getFile`. The default 16 MiB chunk stays under both. A self-hosted
//! `telegram-bot-api --local` server lifts them to 2000 MB.

use super::ChunkBackend;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_ATTEMPTS: u32 = 6;

struct Bot {
    id: i64,
    token: String,
    cooldown_until_ms: AtomicI64,
    uploads: AtomicU64,
    downloads: AtomicU64,
    errors: AtomicU64,
    last_error: Mutex<Option<String>>,
    username: Mutex<Option<String>>,
}

#[derive(Serialize, Deserialize)]
struct Locator {
    b: i64,
    c: i64,
    m: i64,
    f: String,
}

pub struct TelegramBackend {
    api_base: String,
    chat_id: i64,
    local_mode: bool,
    bots: Vec<Arc<Bot>>,
    rr: AtomicUsize,
    http: reqwest::Client,
    file_paths: DashMap<String, (String, Instant)>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// reqwest errors embed the request URL, and our URLs contain the bot token.
fn scrub(e: reqwest::Error) -> anyhow::Error {
    anyhow!("telegram http: {}", e.without_url())
}

impl TelegramBackend {
    pub fn new(api_base: String, bot_tokens: Vec<String>, chat_id: i64, local_mode: bool) -> Result<Self> {
        let bots = bot_tokens
            .into_iter()
            .map(|token| {
                let id = token.split(':').next().unwrap_or("0").parse().unwrap_or(0);
                Arc::new(Bot {
                    id,
                    token,
                    cooldown_until_ms: AtomicI64::new(0),
                    uploads: AtomicU64::new(0),
                    downloads: AtomicU64::new(0),
                    errors: AtomicU64::new(0),
                    last_error: Mutex::new(None),
                    username: Mutex::new(None),
                })
            })
            .collect();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(16)
            .user_agent("tgvault/0.1")
            .build()?;
        Ok(Self {
            api_base: api_base.trim_end_matches('/').to_string(),
            chat_id,
            local_mode,
            bots,
            rr: AtomicUsize::new(0),
            http,
            file_paths: DashMap::new(),
        })
    }

    /// Calls getMe for every bot; logs who we are and fails fast on a bad token.
    pub async fn verify(&self) -> Result<()> {
        for bot in &self.bots {
            let me = self.call(bot, "getMe", None).await.with_context(|| format!("bot {} failed getMe", bot.id))?;
            let name = me.get("username").and_then(Value::as_str).unwrap_or("?").to_string();
            tracing::info!(bot = bot.id, username = %name, "telegram bot ready");
            *bot.username.lock() = Some(name);
        }
        Ok(())
    }

    /// Chats each bot has seen recently (via getUpdates). After adding a bot to a
    /// channel, post any message there and this lists the channel with its id.
    pub async fn discover_chats(&self) -> Result<Vec<(i64, String, String)>> {
        let mut out: Vec<(i64, String, String)> = vec![];
        for bot in &self.bots {
            let updates = self.call_json(bot, "getUpdates", Some(&json!({ "limit": 100, "timeout": 0 })), None).await?;
            for u in updates.as_array().cloned().unwrap_or_default() {
                for path in ["/channel_post/chat", "/my_chat_member/chat", "/message/forward_origin/chat", "/message/forward_from_chat", "/edited_channel_post/chat", "/message/chat"] {
                    if let Some(c) = u.pointer(path) {
                        let id = c.get("id").and_then(Value::as_i64).unwrap_or(0);
                        let title = c
                            .get("title")
                            .or_else(|| c.get("username"))
                            .or_else(|| c.get("first_name"))
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string();
                        let kind = c.get("type").and_then(Value::as_str).unwrap_or("?").to_string();
                        if id != 0 && !out.iter().any(|(i, _, _)| *i == id) {
                            out.push((id, title, kind));
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// End-to-end readiness check: identity, admin rights in the chat, and a real
    /// upload -> download -> delete round trip per bot. Returns (ok, report lines).
    pub async fn diagnose(&self) -> (bool, Vec<String>) {
        let mut lines = vec![];
        let mut ok = true;
        let chat = match self.call_json(&self.bots[0], "getChat", Some(&json!({ "chat_id": self.chat_id })), None).await {
            Ok(c) => {
                lines.push(format!(
                    "ok   chat {}: '{}' ({})",
                    self.chat_id,
                    c.get("title").and_then(Value::as_str).unwrap_or("?"),
                    c.get("type").and_then(Value::as_str).unwrap_or("?")
                ));
                c
            }
            Err(e) => {
                lines.push(format!("FAIL chat {}: {e:#}. Is the first bot in that chat, and is the id right (channels start with -100)?", self.chat_id));
                return (false, lines);
            }
        };
        let is_channel = chat.get("type").and_then(Value::as_str) == Some("channel");
        for (idx, bot) in self.bots.iter().enumerate() {
            let name = match self.call(bot, "getMe", None).await {
                Ok(me) => format!("@{}", me.get("username").and_then(Value::as_str).unwrap_or("?")),
                Err(e) => {
                    ok = false;
                    lines.push(format!("FAIL bot {}: token rejected ({e:#})", bot.id));
                    continue;
                }
            };
            match self.call_json(bot, "getChatMember", Some(&json!({ "chat_id": self.chat_id, "user_id": bot.id })), None).await {
                Ok(m) => {
                    let status = m.get("status").and_then(Value::as_str).unwrap_or("?").to_string();
                    let flag = |k: &str| m.get(k).and_then(Value::as_bool).unwrap_or(false);
                    let mut missing: Vec<&str> = vec![];
                    if status != "administrator" && status != "creator" {
                        missing.push("administrator role");
                    } else {
                        if is_channel && !flag("can_post_messages") {
                            missing.push("Post messages");
                        }
                        if !flag("can_delete_messages") {
                            missing.push("Delete messages");
                        }
                        if !flag("can_pin_messages") && !(is_channel && flag("can_edit_messages")) {
                            missing.push("Pin messages (used for backups)");
                        }
                    }
                    if missing.is_empty() {
                        lines.push(format!("ok   {name}: {status} with the needed rights"));
                    } else {
                        ok = false;
                        lines.push(format!("FAIL {name}: missing {}", missing.join(", ")));
                        continue;
                    }
                }
                Err(e) => {
                    ok = false;
                    lines.push(format!("FAIL {name}: not in the chat ({e:#}). Add it as an administrator."));
                    continue;
                }
            }
            // Round trip with a throwaway file through this specific bot.
            let probe = Bytes::from(format!("tgvault probe {}", bot.id));
            let started = Instant::now();
            let result: Result<()> = async {
                self.rr.store(idx, Ordering::Relaxed);
                let loc = self.put("tgvault-probe.bin", probe.clone()).await?;
                let back = self.get(&loc).await?;
                self.delete(&loc).await?;
                if back != probe {
                    bail!("downloaded bytes differ from uploaded bytes");
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => lines.push(format!("ok   {name}: upload, download and delete work ({} ms)", started.elapsed().as_millis())),
                Err(e) => {
                    ok = false;
                    lines.push(format!("FAIL {name}: round trip failed: {e:#}"));
                }
            }
        }
        (ok, lines)
    }

    fn bot_by_id(&self, id: i64) -> Result<&Arc<Bot>> {
        self.bots
            .iter()
            .find(|b| b.id == id)
            .ok_or_else(|| anyhow!("chunk was uploaded by bot {id}, which is no longer in TGV_BOT_TOKENS"))
    }

    /// Next bot that is not cooling down; if all are, the one that frees up first.
    async fn pick_bot(&self) -> Arc<Bot> {
        let n = self.bots.len();
        let start = self.rr.fetch_add(1, Ordering::Relaxed);
        let now = now_ms();
        for i in 0..n {
            let b = &self.bots[(start + i) % n];
            if b.cooldown_until_ms.load(Ordering::Relaxed) <= now {
                return b.clone();
            }
        }
        let b = self.bots.iter().min_by_key(|b| b.cooldown_until_ms.load(Ordering::Relaxed)).unwrap().clone();
        let wait = (b.cooldown_until_ms.load(Ordering::Relaxed) - now).clamp(0, 60_000) as u64;
        tokio::time::sleep(Duration::from_millis(wait)).await;
        b
    }

    fn note_error(bot: &Bot, msg: &str) {
        bot.errors.fetch_add(1, Ordering::Relaxed);
        *bot.last_error.lock() = Some(msg.chars().take(200).collect());
    }

    async fn call(&self, bot: &Bot, method: &str, body: Option<&(dyn Fn() -> reqwest::multipart::Form + Send + Sync)>) -> Result<Value> {
        self.call_json(bot, method, None, body).await
    }

    async fn call_json(
        &self,
        bot: &Bot,
        method: &str,
        params: Option<&Value>,
        form: Option<&(dyn Fn() -> reqwest::multipart::Form + Send + Sync)>,
    ) -> Result<Value> {
        let url = format!("{}/bot{}/{}", self.api_base, bot.token, method);
        let mut backoff = Duration::from_millis(500);
        for attempt in 1..=MAX_ATTEMPTS {
            let req = self.http.post(&url);
            let req = match (form, params) {
                (Some(f), _) => req.multipart(f()),
                (None, Some(p)) => req.json(p),
                (None, None) => req,
            };
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    let err = scrub(e);
                    Self::note_error(bot, &err.to_string());
                    if attempt == MAX_ATTEMPTS {
                        return Err(err);
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(20));
                    continue;
                }
            };
            let status = resp.status();
            let v: Value = resp.json().await.map_err(scrub)?;
            if v.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            let desc = v.get("description").and_then(Value::as_str).unwrap_or("unknown error").to_string();
            let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(status.as_u16() as i64);
            Self::note_error(bot, &desc);
            if code == 429 {
                let retry = v.pointer("/parameters/retry_after").and_then(Value::as_u64).unwrap_or(5).min(120);
                bot.cooldown_until_ms.store(now_ms() + retry as i64 * 1000, Ordering::Relaxed);
                tracing::warn!(bot = bot.id, retry, "telegram flood control");
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(retry)).await;
                    continue;
                }
            } else if code >= 500 && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(20));
                continue;
            }
            bail!("telegram {method} failed ({code}): {desc}");
        }
        bail!("telegram {method}: retries exhausted")
    }

    async fn file_path(&self, bot: &Bot, file_id: &str, fresh: bool) -> Result<String> {
        if !fresh {
            if let Some(e) = self.file_paths.get(file_id) {
                if e.1.elapsed() < Duration::from_secs(50 * 60) {
                    return Ok(e.0.clone());
                }
            }
        }
        let r = self.call_json(bot, "getFile", Some(&json!({ "file_id": file_id })), None).await?;
        let path = r.get("file_path").and_then(Value::as_str).ok_or_else(|| anyhow!("getFile returned no file_path"))?.to_string();
        self.file_paths.insert(file_id.to_string(), (path.clone(), Instant::now()));
        Ok(path)
    }

    async fn download(&self, bot: &Bot, path: &str) -> Result<Bytes> {
        if self.local_mode && std::path::Path::new(path).is_absolute() {
            return Ok(Bytes::from(tokio::fs::read(path).await.context("read local bot-api file")?));
        }
        let url = format!("{}/file/bot{}/{}", self.api_base, bot.token, path);
        let resp = self.http.get(&url).send().await.map_err(scrub)?;
        if !resp.status().is_success() {
            bail!("telegram file download returned HTTP {}", resp.status().as_u16());
        }
        resp.bytes().await.map_err(scrub)
    }

    fn parse(locator: &str) -> Result<Locator> {
        serde_json::from_str(locator).context("bad telegram locator")
    }
}

#[async_trait]
impl ChunkBackend for TelegramBackend {
    fn kind(&self) -> &'static str {
        "telegram"
    }

    async fn put(&self, name: &str, data: Bytes) -> Result<String> {
        let bot = self.pick_bot().await;
        let chat = self.chat_id.to_string();
        let name = name.to_string();
        let len = data.len() as u64;
        let make = || {
            let part = reqwest::multipart::Part::stream_with_length(reqwest::Body::from(data.clone()), len)
                .file_name(name.clone())
                .mime_str("application/octet-stream")
                .expect("static mime is valid");
            reqwest::multipart::Form::new()
                .text("chat_id", chat.clone())
                .text("disable_notification", "true")
                .text("disable_content_type_detection", "true")
                .part("document", part)
        };
        let r = self.call_json(&bot, "sendDocument", None, Some(&make)).await?;
        let m = r.get("message_id").and_then(Value::as_i64).ok_or_else(|| anyhow!("sendDocument: no message_id"))?;
        let f = r
            .pointer("/document/file_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("sendDocument: no document in reply"))?
            .to_string();
        bot.uploads.fetch_add(1, Ordering::Relaxed);
        Ok(serde_json::to_string(&Locator { b: bot.id, c: self.chat_id, m, f })?)
    }

    async fn get(&self, locator: &str) -> Result<Bytes> {
        let loc = Self::parse(locator)?;
        let bot = self.bot_by_id(loc.b)?.clone();
        let path = self.file_path(&bot, &loc.f, false).await?;
        let data = match self.download(&bot, &path).await {
            Ok(d) => d,
            Err(_) => {
                // file_path links expire; refresh once.
                let path = self.file_path(&bot, &loc.f, true).await?;
                self.download(&bot, &path).await?
            }
        };
        bot.downloads.fetch_add(1, Ordering::Relaxed);
        Ok(data)
    }

    async fn delete(&self, locator: &str) -> Result<()> {
        let loc = Self::parse(locator)?;
        let bot = self.bot_by_id(loc.b)?.clone();
        match self.call_json(&bot, "deleteMessage", Some(&json!({ "chat_id": loc.c, "message_id": loc.m })), None).await {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("message to delete not found") => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn mark_backup(&self, locator: &str) -> Result<()> {
        let loc = Self::parse(locator)?;
        let bot = self.bot_by_id(loc.b)?.clone();
        self.call_json(
            &bot,
            "pinChatMessage",
            Some(&json!({ "chat_id": loc.c, "message_id": loc.m, "disable_notification": true })),
            None,
        )
        .await?;
        Ok(())
    }

    async fn latest_backup(&self) -> Result<Option<String>> {
        let bot = self.bots[0].clone();
        let chat = self.call_json(&bot, "getChat", Some(&json!({ "chat_id": self.chat_id })), None).await?;
        let Some(pinned) = chat.get("pinned_message") else { return Ok(None) };
        let (Some(m), Some(f)) = (
            pinned.get("message_id").and_then(Value::as_i64),
            pinned.pointer("/document/file_id").and_then(Value::as_str),
        ) else {
            return Ok(None);
        };
        Ok(Some(serde_json::to_string(&Locator { b: bot.id, c: self.chat_id, m, f: f.to_string() })?))
    }

    async fn health(&self) -> Value {
        let now = now_ms();
        let bots: Vec<Value> = self
            .bots
            .iter()
            .map(|b| {
                let cd = (b.cooldown_until_ms.load(Ordering::Relaxed) - now).max(0);
                json!({
                    "id": b.id,
                    "username": b.username.lock().clone(),
                    "cooldown_ms": cd,
                    "uploads": b.uploads.load(Ordering::Relaxed),
                    "downloads": b.downloads.load(Ordering::Relaxed),
                    "errors": b.errors.load(Ordering::Relaxed),
                    "last_error": b.last_error.lock().clone(),
                })
            })
            .collect();
        json!({ "kind": "telegram", "ok": true, "chat_id": self.chat_id, "local_mode": self.local_mode, "bots": bots })
    }
}
