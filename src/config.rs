//! Runtime configuration, read once from environment variables (see `.env.example`).

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum BackendConfig {
    Telegram {
        api_base: String,
        bot_tokens: Vec<String>,
        chat_id: i64,
        /// When the bot API runs in `--local` mode, `getFile` returns a path on disk.
        local_mode: bool,
    },
    Local {
        dir: PathBuf,
    },
}

#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub public_url: String,
    pub data_dir: PathBuf,
    pub backend: BackendConfig,
    /// Key-encryption keys, by version. The active one wraps new data keys.
    pub master_keys: BTreeMap<u32, [u8; 32]>,
    pub active_key: u32,
    /// Separate secret for HMAC: API-key pepper, presigned URLs, sessions.
    pub server_secret: [u8; 32],
    pub admin_password: Option<String>,
    pub admin_password_hash: Option<String>,
    pub chunk_size: usize,
    pub max_upload_bytes: u64,
    pub cache_bytes: usize,
    pub upload_concurrency: usize,
    pub trash_days: i64,
    pub default_key_rpm: u32,
    pub public_rpm: u32,
    pub trust_proxy: bool,
    pub cookie_secure: bool,
    pub backup_interval_hours: u64,
    pub session_hours: i64,
}

impl std::fmt::Debug for Config {
    // Never print key material, even by accident in a panic message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("public_url", &self.public_url)
            .field("data_dir", &self.data_dir)
            .field("active_key", &self.active_key)
            .field("chunk_size", &self.chunk_size)
            .finish_non_exhaustive()
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> Result<T> {
    match env(name) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| anyhow!("{name} has an invalid value")),
    }
}

pub fn decode_key(b64: &str, what: &str) -> Result<[u8; 32]> {
    let raw = B64.decode(b64.trim()).with_context(|| format!("{what} is not valid base64"))?;
    raw.try_into().map_err(|_| anyhow!("{what} must decode to exactly 32 bytes (use `openssl rand -base64 32`)"))
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind: SocketAddr = env_or("TGV_BIND", "127.0.0.1:8787".parse().unwrap())?;
        let public_url = env("TGV_PUBLIC_URL").unwrap_or_else(|| format!("http://{bind}"));
        let public_url = public_url.trim_end_matches('/').to_string();
        let data_dir = PathBuf::from(env("TGV_DATA_DIR").unwrap_or_else(|| "./data".into()));

        let backend = match env("TGV_BACKEND").as_deref().unwrap_or("telegram") {
            "telegram" => {
                let tokens: Vec<String> = env("TGV_BOT_TOKENS")
                    .ok_or_else(|| anyhow!("TGV_BOT_TOKENS is required for the telegram backend"))?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if tokens.is_empty() {
                    bail!("TGV_BOT_TOKENS has no tokens");
                }
                for t in &tokens {
                    // "<numeric id>:<secret>" — the id is what we persist per chunk.
                    let ok = t.split_once(':').map(|(id, s)| id.parse::<i64>().is_ok() && !s.is_empty());
                    if ok != Some(true) {
                        bail!("a bot token in TGV_BOT_TOKENS is malformed");
                    }
                }
                BackendConfig::Telegram {
                    api_base: env("TGV_TG_API_BASE").unwrap_or_else(|| "https://api.telegram.org".into()),
                    bot_tokens: tokens,
                    chat_id: env("TGV_CHAT_ID")
                        .ok_or_else(|| anyhow!("TGV_CHAT_ID is required (channel id, e.g. -100123...)"))?
                        .parse()
                        .context("TGV_CHAT_ID must be a number")?,
                    local_mode: env_or("TGV_TG_LOCAL_MODE", false)?,
                }
            }
            "local" => BackendConfig::Local {
                dir: env("TGV_LOCAL_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir.join("chunks")),
            },
            other => bail!("unknown TGV_BACKEND '{other}' (telegram|local)"),
        };

        let mut master_keys = BTreeMap::new();
        let raw = env("TGV_MASTER_KEYS").ok_or_else(|| anyhow!("TGV_MASTER_KEYS is required, e.g. 1:<base64 32 bytes>"))?;
        for part in raw.split(',') {
            let (v, k) = part.split_once(':').ok_or_else(|| anyhow!("TGV_MASTER_KEYS entries look like <version>:<base64>"))?;
            let v: u32 = v.trim().parse().context("key version must be a number")?;
            master_keys.insert(v, decode_key(k, "master key")?);
        }
        let active_key = env_or("TGV_ACTIVE_KEY", *master_keys.keys().max().unwrap())?;
        if !master_keys.contains_key(&active_key) {
            bail!("TGV_ACTIVE_KEY={active_key} is not present in TGV_MASTER_KEYS");
        }
        let server_secret = decode_key(
            &env("TGV_SERVER_SECRET").ok_or_else(|| anyhow!("TGV_SERVER_SECRET is required (base64, 32 bytes)"))?,
            "TGV_SERVER_SECRET",
        )?;
        if master_keys.values().any(|k| k == &server_secret) {
            bail!("TGV_SERVER_SECRET must differ from every master key");
        }

        let chunk_mib: usize = env_or("TGV_CHUNK_MIB", 16)?;
        if chunk_mib == 0 || chunk_mib > 1900 {
            bail!("TGV_CHUNK_MIB must be between 1 and 1900");
        }
        let admin_password = env("TGV_ADMIN_PASSWORD");
        if let Some(p) = &admin_password {
            if p.chars().count() < 12 {
                bail!("TGV_ADMIN_PASSWORD must be at least 12 characters");
            }
        }

        Ok(Self {
            cookie_secure: env_or("TGV_COOKIE_SECURE", public_url.starts_with("https://"))?,
            bind,
            public_url,
            data_dir,
            backend,
            master_keys,
            active_key,
            server_secret,
            admin_password,
            admin_password_hash: env("TGV_ADMIN_PASSWORD_HASH"),
            chunk_size: chunk_mib * 1024 * 1024,
            max_upload_bytes: env_or::<u64>("TGV_MAX_UPLOAD_GIB", 64)? * 1024 * 1024 * 1024,
            cache_bytes: env_or::<usize>("TGV_CACHE_MIB", 256)? * 1024 * 1024,
            upload_concurrency: env_or("TGV_UPLOAD_CONCURRENCY", 4)?,
            trash_days: env_or("TGV_TRASH_DAYS", 30)?,
            default_key_rpm: env_or("TGV_DEFAULT_KEY_RPM", 600)?,
            public_rpm: env_or("TGV_PUBLIC_RPM", 300)?,
            trust_proxy: env_or("TGV_TRUST_PROXY", false)?,
            backup_interval_hours: env_or("TGV_BACKUP_INTERVAL_HOURS", 6)?,
            session_hours: env_or("TGV_SESSION_HOURS", 12)?,
        })
    }
}
