//! Background maintenance: lifecycle expiry, trash purge, stale uploads, chunk GC,
//! encrypted metadata backups, and key re-wrapping.

use crate::db::now;
use crate::error::ApiResult;
use crate::storage::new_id;
use crate::AppState;
use bytes::Bytes;
use rusqlite::params;
use serde_json::{json, Value};
use std::time::Duration;

pub fn spawn(state: AppState) {
    let s = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            match gc(&s).await {
                Ok(v) => tracing::debug!(report = %v, "gc pass"),
                Err(e) => tracing::warn!("gc failed: {e}"),
            }
            s.limiter.sweep();
            let t = now();
            s.sessions.retain(|_, sess| sess.expires_at > t);
            // Forget finished import jobs after a day.
            s.imports.retain(|_, j| {
                let f = j.finished_at.load(std::sync::atomic::Ordering::Relaxed) as i64;
                f == 0 || t - f < 86400
            });
        }
    });
    if state.cfg.backup_interval_hours > 0 {
        let s = state.clone();
        tokio::spawn(async move {
            let every = Duration::from_secs(s.cfg.backup_interval_hours * 3600);
            let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(120), every);
            loop {
                tick.tick().await;
                if let Err(e) = backup(&s).await {
                    tracing::warn!("scheduled backup failed: {e}");
                }
            }
        });
    }
}

/// One full maintenance pass. Safe to run concurrently with traffic.
pub async fn gc(state: &AppState) -> ApiResult<Value> {
    let trash_cutoff = now() - state.cfg.trash_days * 86400;
    let upload_cutoff = now() - crate::routes::multipart::UPLOAD_TTL_SECS;
    // 1. Objects past their TTL or long in the trash are deleted for good.
    let doomed: Vec<String> = state
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT id FROM objects WHERE (expires_at IS NOT NULL AND expires_at <= ?1) OR (deleted_at IS NOT NULL AND deleted_at <= ?2) LIMIT 5000",
            )?;
            let v = st.query_map(params![now(), trash_cutoff], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    let expired = doomed.len();
    for id in doomed {
        state.engine.delete_object(&id, true).await?;
    }
    // 2. Abandoned multipart uploads, and single PUTs that died mid-stream.
    let stale_uploads: Vec<(String, String)> = state
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT id, blob_id FROM uploads WHERE created_at <= ?1")?;
            let v = st.query_map([upload_cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    for (uid, blob) in &stale_uploads {
        crate::routes::multipart::abort_upload(state, uid, blob).await?;
    }
    // 3. Orphan blobs -> chunk deletion queue.
    let blobs: usize = state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "UPDATE blobs SET state = 'orphan' WHERE state = 'uploading' AND refcount = 0 AND created_at <= ?1
                   AND id NOT IN (SELECT blob_id FROM uploads)",
                [upload_cutoff],
            )?;
            let ids: Vec<String> = {
                let mut st = tx.prepare("SELECT id FROM blobs WHERE state = 'orphan' AND refcount <= 0 LIMIT 2000")?;
                let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                v
            };
            for id in &ids {
                tx.execute("INSERT INTO dead_chunks (backend, locator) SELECT backend, locator FROM chunks WHERE blob_id = ?1", [id])?;
                tx.execute("DELETE FROM chunks WHERE blob_id = ?1", [id])?;
                tx.execute("DELETE FROM blobs WHERE id = ?1", [id])?;
            }
            tx.commit()?;
            Ok(ids.len())
        })
        .await?;
    // 4. Delete queued chunks from the backend (Telegram deleteMessage).
    let dead: Vec<(i64, String, String)> = state
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT id, backend, locator FROM dead_chunks WHERE attempts < 8 ORDER BY id LIMIT 500")?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    let backend = state.backend.clone();
    let (mut removed, mut failed) = (0, 0);
    for (id, kind, locator) in dead {
        if kind != backend.kind() {
            continue; // written by another backend configuration; leave it queued
        }
        state.engine.cache.forget(&locator);
        let ok = backend.delete(&locator).await.is_ok();
        if ok { removed += 1 } else { failed += 1 }
        state
            .db
            .call(move |c| {
                if ok {
                    c.execute("DELETE FROM dead_chunks WHERE id = ?1", [id])?;
                } else {
                    c.execute("UPDATE dead_chunks SET attempts = attempts + 1 WHERE id = ?1", [id])?;
                }
                Ok(())
            })
            .await?;
    }
    Ok(json!({ "expired_objects": expired, "stale_uploads": stale_uploads.len(), "orphan_blobs": blobs, "chunks_deleted": removed, "chunk_delete_failures": failed }))
}

/// Snapshot the SQLite file, seal it with the active master key, store it in the
/// backend and mark it (Telegram: pin) so `tgvault restore` can find it with
/// nothing but the bot tokens and master keys.
pub async fn backup(state: &AppState) -> ApiResult<Value> {
    let dir = state.cfg.data_dir.join("backups");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| crate::error::ApiError::Internal(e.to_string()))?;
    let snap = dir.join(format!("snap-{}.db", new_id()));
    let snap_s = snap.to_string_lossy().to_string();
    state
        .db
        .call(move |c| {
            c.execute("VACUUM INTO ?1", [snap_s])?;
            Ok(())
        })
        .await?;
    let raw = tokio::fs::read(&snap).await.map_err(|e| crate::error::ApiError::Internal(e.to_string()))?;
    let _ = tokio::fs::remove_file(&snap).await;
    let sealed = state.keys.seal(&raw, b"tgvault-backup")?;
    let id = new_id();
    let name = format!("tgvault-backup-{}.bin", now());
    let size = sealed.len() as i64;
    let local_copy = dir.join(&name);
    tokio::fs::write(&local_copy, &sealed).await.map_err(|e| crate::error::ApiError::Internal(e.to_string()))?;
    let locator = state.backend.put(&name, Bytes::from(sealed)).await.map_err(|e| crate::error::ApiError::Backend(format!("{e:#}")))?;
    if let Err(e) = state.backend.mark_backup(&locator).await {
        tracing::warn!("could not mark backup (is the bot allowed to pin messages?): {e:#}");
    }
    let (bid, loc, kind) = (id.clone(), locator.clone(), state.backend.kind().to_string());
    state
        .db
        .call(move |c| {
            c.execute("INSERT INTO backups (id, created_at, size, backend, locator) VALUES (?1, ?2, ?3, ?4, ?5)", params![bid, now(), size, kind, loc])?;
            Ok(())
        })
        .await?;
    // Keep the 10 newest local copies.
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        let mut files = vec![];
        while let Ok(Some(e)) = rd.next_entry().await {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("tgvault-backup-") {
                files.push(n);
            }
        }
        files.sort();
        let excess = files.len().saturating_sub(10);
        for f in files.into_iter().take(excess) {
            let _ = tokio::fs::remove_file(dir.join(f)).await;
        }
    }
    tracing::info!(size, "metadata backup stored");
    Ok(json!({ "id": id, "size": size, "local_copy": local_copy.file_name().map(|f| f.to_string_lossy().to_string()) }))
}

/// Re-wrap every data key with the active master key. Cheap: no chunk moves.
pub async fn rewrap(state: &AppState) -> ApiResult<Value> {
    let active = state.keys.active;
    let rows: Vec<(String, Vec<u8>, u32)> = state
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT id, wrapped_dek, kek_version FROM blobs WHERE kek_version != ?1")?;
            let v = st.query_map([active], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            Ok(v)
        })
        .await?;
    let mut updates = Vec::with_capacity(rows.len());
    for (id, wrapped, ver) in rows {
        let dek = state.keys.unwrap_dek(&wrapped, ver, &id)?;
        let (w, v) = state.keys.wrap_dek(&dek, &id)?;
        updates.push((id, w, v));
    }
    let n = updates.len();
    state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            for (id, w, v) in updates {
                tx.execute("UPDATE blobs SET wrapped_dek = ?1, kek_version = ?2 WHERE id = ?3", params![w, v, id])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(json!({ "rewrapped": n, "active_key": active }))
}
