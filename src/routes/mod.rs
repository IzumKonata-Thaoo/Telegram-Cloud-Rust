pub mod admin;
pub mod buckets;
pub mod imports;
pub mod keys;
pub mod multipart;
pub mod objects;
pub mod panel;
pub mod public;
pub mod serve;
pub mod shares;

use crate::error::{ApiError, ApiResult};
use crate::storage::Bucket;
use crate::AppState;
use axum::routing::{delete, get, post, put};
use axum::Router;
use rusqlite::OptionalExtension;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/auth/login", post(panel::login))
        .route("/auth/logout", post(panel::logout))
        .route("/auth/me", get(panel::me))
        .route("/buckets", get(buckets::list).post(buckets::create))
        .route("/buckets/{bucket}", get(buckets::get_one).patch(buckets::update).delete(buckets::remove))
        .route("/buckets/{bucket}/objects", get(objects::list))
        .route(
            "/buckets/{bucket}/objects/{*key}",
            put(objects::upload).get(objects::download).head(objects::download).delete(objects::remove),
        )
        .route("/buckets/{bucket}/copy", post(objects::copy))
        .route("/buckets/{bucket}/link-by-hash", post(objects::link_by_hash))
        .route("/buckets/{bucket}/trash", get(objects::trash).delete(objects::empty_trash))
        .route("/objects/{id}", get(objects::by_id))
        .route("/objects/{id}/restore", post(objects::restore))
        .route("/buckets/{bucket}/import", post(imports::start))
        .route("/imports", get(imports::list))
        .route("/imports/{id}", get(imports::status))
        .route("/buckets/{bucket}/uploads", post(multipart::init).get(multipart::list))
        .route("/uploads/{id}", get(multipart::status).delete(multipart::abort))
        .route("/uploads/{id}/parts/{n}", put(multipart::upload_part))
        .route("/uploads/{id}/complete", post(multipart::complete))
        .route("/presign", post(public::presign))
        .route("/shares", get(shares::list).post(shares::create))
        .route("/shares/{id}", delete(shares::revoke))
        .route("/keys", get(keys::list).post(keys::create))
        .route("/keys/{id}", delete(keys::revoke))
        .route("/admin/stats", get(admin::stats))
        .route("/admin/audit", get(admin::audit))
        .route("/admin/backups", get(admin::list_backups).post(admin::backup_now))
        .route("/admin/gc", post(admin::gc_now))
        .route("/admin/rewrap", post(admin::rewrap))
        .route("/openapi.json", get(panel::openapi));

    Router::new()
        .nest("/api/v1", api)
        .route("/healthz", get(admin::healthz))
        .route("/pub/{bucket}/{*key}", get(public::public_bucket).head(public::public_bucket).options(public::preflight))
        .route("/p/{bucket}/{*key}", get(public::presigned_get).head(public::presigned_get).put(public::presigned_put).options(public::preflight))
        .route("/s/{token}", get(shares::landing))
        .route("/s/{token}/unlock", post(shares::unlock))
        .route("/s/{token}/raw", get(shares::raw).head(shares::raw))
        .route("/", get(panel::index))
        .route("/docs", get(panel::docs))
        .route("/assets/{file}", get(panel::asset))
        .with_state(state)
}

pub fn validate_bucket_name(name: &str) -> ApiResult<()> {
    let ok = (3..=63).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(ApiError::bad("bucket names are 3-63 characters: lowercase letters, digits and '-', not starting or ending with '-'"))
    }
}

/// Object keys are opaque UTF-8 up to 1024 bytes. They never touch a filesystem,
/// but control characters and dot segments are refused so keys stay printable and
/// can be mirrored to disk safely by clients.
pub fn validate_key(key: &str) -> ApiResult<()> {
    if key.is_empty() || key.len() > 1024 {
        return Err(ApiError::bad("object keys must be 1-1024 bytes"));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad("object keys cannot contain control characters"));
    }
    if key.starts_with('/') || key.split('/').any(|seg| seg == "." || seg == "..") {
        return Err(ApiError::bad("object keys cannot start with '/' or contain '.' or '..' segments"));
    }
    if key.contains('\\') {
        return Err(ApiError::bad("object keys cannot contain backslashes"));
    }
    Ok(())
}

pub async fn bucket_by_name(state: &AppState, name: &str) -> ApiResult<Bucket> {
    let n = name.to_string();
    state
        .db
        .call(move |c| {
            let sql = format!("SELECT {} FROM buckets WHERE name = ?1", Bucket::COLS);
            Ok(c.query_row(&sql, [n], Bucket::from_row).optional()?)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("bucket"))
}

pub fn guess_type(key: &str, given: Option<&str>) -> String {
    match given.map(str::trim).filter(|s| !s.is_empty() && *s != "application/octet-stream") {
        Some(ct) if ct.len() <= 200 && ct.parse::<mime_guess::mime::Mime>().is_ok() => ct.to_string(),
        _ => mime_guess::from_path(key).first_or_octet_stream().essence_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert!(validate_key("videos/2026/a b.mp4").is_ok());
        assert!(validate_key("../etc/passwd").is_err());
        assert!(validate_key("a/../b").is_err());
        assert!(validate_key("/abs").is_err());
        assert!(validate_key("a\nb").is_err());
        assert!(validate_key("a\\b").is_err());
        assert!(validate_key("").is_err());
        assert!(validate_key("...").is_ok());
    }

    #[test]
    fn buckets() {
        assert!(validate_bucket_name("media-01").is_ok());
        assert!(validate_bucket_name("Media").is_err());
        assert!(validate_bucket_name("-x-").is_err());
        assert!(validate_bucket_name("ab").is_err());
    }
}
