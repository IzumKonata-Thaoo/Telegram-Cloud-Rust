mod auth;
mod backend;
mod cache;
mod config;
mod crypto;
mod db;
mod error;
mod fetch;
mod jobs;
mod routes;
mod storage;

use anyhow::{bail, Context, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
use axum::http::{header, HeaderName, HeaderValue};
use backend::{local::LocalBackend, telegram::TelegramBackend, ChunkBackend};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use config::{BackendConfig, Config};
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

pub struct AppStateInner {
    pub cfg: Config,
    pub db: db::Db,
    pub engine: storage::Engine,
    pub keys: Arc<crypto::Keyring>,
    pub backend: Arc<dyn ChunkBackend>,
    pub limiter: auth::RateLimiter,
    pub sessions: DashMap<String, auth::Session>,
    pub admin_hash: Option<String>,
    pub started: Instant,
    pub imports: DashMap<String, Arc<routes::imports::Job>>,
    pub import_slots: tokio::sync::Semaphore,
    pub fetcher: reqwest::Client,
}

pub type AppState = Arc<AppStateInner>;

async fn make_backend(cfg: &Config) -> Result<Arc<dyn ChunkBackend>> {
    Ok(match &cfg.backend {
        BackendConfig::Telegram { api_base, bot_tokens, chat_id, local_mode } => {
            let tg = TelegramBackend::new(api_base.clone(), bot_tokens.clone(), *chat_id, *local_mode)?;
            tg.verify().await?;
            Arc::new(tg)
        }
        BackendConfig::Local { dir } => Arc::new(LocalBackend::new(dir.clone())?),
    })
}

fn usage() -> ! {
    eprintln!(
        "tgvault {ver}

USAGE:
  tgvault [serve]                     run the server (configured by TGV_* env vars)
  tgvault gen-secrets                 print fresh TGV_MASTER_KEYS / TGV_SERVER_SECRET values
  tgvault hash-password               read a password on stdin, print an argon2 hash for TGV_ADMIN_PASSWORD_HASH
  tgvault decrypt-backup <in> <out>   decrypt a sealed metadata backup into a SQLite file
  tgvault restore <out>               fetch the newest pinned backup from the backend and decrypt it
  tgvault telegram-chats              list chats/channels your bots have seen (to find TGV_CHAT_ID)
  tgvault telegram-check              verify tokens, admin rights and a real upload/download/delete",
        ver = env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "tgvault=info,tower_http=warn".into()))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => serve().await,
        Some("gen-secrets") => {
            println!("TGV_MASTER_KEYS=1:{}", B64.encode(crypto::random_bytes::<32>()));
            println!("TGV_SERVER_SECRET={}", B64.encode(crypto::random_bytes::<32>()));
            Ok(())
        }
        Some("hash-password") => {
            let mut pw = String::new();
            std::io::stdin().read_line(&mut pw)?;
            let pw = pw.trim_end_matches(['\r', '\n']);
            if pw.chars().count() < 12 {
                bail!("use at least 12 characters");
            }
            let salt = SaltString::generate(&mut OsRng);
            let h = argon2::Argon2::default().hash_password(pw.as_bytes(), &salt).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{h}");
            Ok(())
        }
        Some("decrypt-backup") if args.len() == 3 => {
            let cfg = Config::from_env()?;
            let keys = crypto::Keyring::new(&cfg.master_keys, cfg.active_key, &cfg.server_secret);
            let sealed = std::fs::read(&args[1]).with_context(|| format!("read {}", args[1]))?;
            std::fs::write(&args[2], keys.open(&sealed, b"tgvault-backup")?)?;
            println!("wrote {}", args[2]);
            Ok(())
        }
        Some("restore") if args.len() == 2 => {
            let cfg = Config::from_env()?;
            let keys = crypto::Keyring::new(&cfg.master_keys, cfg.active_key, &cfg.server_secret);
            let backend = make_backend(&cfg).await?;
            let loc = backend.latest_backup().await?.context("no pinned/marked backup found in the backend")?;
            let sealed = backend.get(&loc).await?;
            std::fs::write(&args[1], keys.open(&sealed, b"tgvault-backup")?)?;
            println!("restored metadata to {} — stop the server, then replace tgvault.db with it", args[1]);
            Ok(())
        }
        Some("telegram-chats") => {
            let tokens: Vec<String> = std::env::var("TGV_BOT_TOKENS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if tokens.is_empty() {
                bail!("set TGV_BOT_TOKENS first");
            }
            let api = std::env::var("TGV_TG_API_BASE").unwrap_or_else(|_| "https://api.telegram.org".into());
            let tg = TelegramBackend::new(api, tokens, 0, false)?;
            tg.verify().await?;
            let chats = tg.discover_chats().await?;
            if chats.is_empty() {
                println!("No chats seen yet. Add the bot to your private channel as an administrator, post any message in the channel, then run this again.");
            }
            for (id, title, kind) in chats {
                println!("{id:>16}  {kind:<10}  {title}");
            }
            Ok(())
        }
        Some("telegram-check") => {
            let cfg = Config::from_env()?;
            let BackendConfig::Telegram { api_base, bot_tokens, chat_id, local_mode } = &cfg.backend else {
                bail!("TGV_BACKEND is not telegram");
            };
            let tg = TelegramBackend::new(api_base.clone(), bot_tokens.clone(), *chat_id, *local_mode)?;
            let (ok, lines) = tg.diagnose().await;
            for l in lines {
                println!("{l}");
            }
            if !ok {
                bail!("Telegram setup is not ready yet; fix the FAIL lines above");
            }
            println!("Telegram setup is ready.");
            Ok(())
        }
        _ => usage(),
    }
}

async fn serve() -> Result<()> {
    let cfg = Config::from_env()?;
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("create {}", cfg.data_dir.display()))?;
    let db = db::Db::open(&cfg.data_dir.join("tgvault.db"))?;
    let keys = Arc::new(crypto::Keyring::new(&cfg.master_keys, cfg.active_key, &cfg.server_secret));
    let backend = make_backend(&cfg).await?;
    let cache = cache::ChunkCache::new(backend.clone(), cfg.cache_bytes);
    let engine = storage::Engine::new(db.clone(), cache, keys.clone(), cfg.chunk_size, cfg.max_upload_bytes, cfg.upload_concurrency);

    let admin_hash = match (&cfg.admin_password_hash, &cfg.admin_password) {
        (Some(h), _) => {
            if argon2::password_hash::PasswordHash::new(h).is_err() {
                bail!("TGV_ADMIN_PASSWORD_HASH is not a valid argon2 PHC string (generate one with `tgvault hash-password`)");
            }
            Some(h.clone())
        }
        (None, Some(pw)) => {
            let salt = SaltString::generate(&mut OsRng);
            Some(argon2::Argon2::default().hash_password(pw.as_bytes(), &salt).map_err(|e| anyhow::anyhow!("{e}"))?.to_string())
        }
        (None, None) => {
            tracing::warn!("no TGV_ADMIN_PASSWORD set: the web panel login is disabled (API keys still work)");
            None
        }
    };
    if !cfg.cookie_secure {
        tracing::warn!("session cookies are not marked Secure; serve the panel over HTTPS in production (TGV_PUBLIC_URL=https://...)");
    }

    let state: AppState = Arc::new(AppStateInner {
        cfg: cfg.clone(),
        db,
        engine,
        keys,
        backend,
        limiter: auth::RateLimiter::new(),
        sessions: DashMap::new(),
        admin_hash,
        started: Instant::now(),
        imports: DashMap::new(),
        import_slots: tokio::sync::Semaphore::new(2),
        fetcher: fetch::client()?,
    });
    jobs::spawn(state.clone());

    let mut app = routes::router(state)
        .layer(SetResponseHeaderLayer::if_not_present(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::if_not_present(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin")))
        .layer(SetResponseHeaderLayer::if_not_present(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http());
    if cfg.cookie_secure {
        app = app.layer(SetResponseHeaderLayer::if_not_present(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ));
    }

    let listener = tokio::net::TcpListener::bind(cfg.bind).await.with_context(|| format!("bind {}", cfg.bind))?;
    tracing::info!("tgvault listening on {} (public url {})", cfg.bind, cfg.public_url);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}
