//! Shared response builder for every route that returns object bytes:
//! Range/206, conditional requests, safe content types, download names.

use crate::error::{ApiError, ApiResult};
use crate::storage::Object;
use crate::AppState;
use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// Parse a single `bytes=` range against `size`. Returns Ok(None) for "whole body".
/// Multi-range requests are answered with the full body (allowed by RFC 9110).
pub fn parse_range(h: Option<&str>, size: u64) -> ApiResult<Option<(u64, u64)>> {
    let Some(h) = h else { return Ok(None) };
    let Some(spec) = h.trim().strip_prefix("bytes=") else { return Ok(None) };
    if spec.contains(',') {
        return Ok(None);
    }
    let (a, b) = spec.split_once('-').ok_or(ApiError::RangeNotSatisfiable { size })?;
    let (a, b) = (a.trim(), b.trim());
    let bad = ApiError::RangeNotSatisfiable { size };
    let (start, end_incl) = if a.is_empty() {
        // suffix: last N bytes
        let n: u64 = b.parse().map_err(|_| ApiError::RangeNotSatisfiable { size })?;
        if n == 0 || size == 0 {
            return Err(bad);
        }
        (size.saturating_sub(n), size - 1)
    } else {
        let start: u64 = a.parse().map_err(|_| ApiError::RangeNotSatisfiable { size })?;
        if start >= size {
            return Err(bad);
        }
        let end = if b.is_empty() { size - 1 } else { b.parse::<u64>().map_err(|_| ApiError::RangeNotSatisfiable { size })?.min(size - 1) };
        if end < start {
            return Err(bad);
        }
        (start, end)
    };
    Ok(Some((start, end_incl + 1)))
}

/// Types a browser would execute or render as an active document. Served inline
/// from our origin they could script against the panel session (stored XSS), so
/// they are always downloaded and additionally sandboxed by CSP.
fn is_active_type(ct: &str) -> bool {
    let ct = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    matches!(
        ct.as_str(),
        "text/html" | "application/xhtml+xml" | "image/svg+xml" | "text/xml" | "application/xml" | "text/javascript"
            | "application/javascript" | "application/x-javascript" | "text/xsl" | "application/pdf" | "multipart/x-mixed-replace"
    ) || ct.ends_with("+xml")
        || ct.is_empty()
}

const FILENAME_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'.').remove(b'-').remove(b'_');

fn disposition(key: &str, attachment: bool) -> HeaderValue {
    let name = key.rsplit('/').next().unwrap_or("download");
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || ".-_ ".contains(c) { c } else { '_' }).collect();
    let kind = if attachment { "attachment" } else { "inline" };
    let v = format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{}", utf8_percent_encode(name, FILENAME_SET));
    HeaderValue::from_str(&v).unwrap_or(HeaderValue::from_static("attachment"))
}

pub struct ServeOpts {
    pub force_download: bool,
    pub cache_control: &'static str,
}

pub async fn serve_object(state: &AppState, obj: &Object, method: &Method, headers: &HeaderMap, opts: ServeOpts) -> ApiResult<Response> {
    let size = obj.size.max(0) as u64;
    let etag = format!("\"{}\"", obj.etag);
    let last_mod = obj.created_at;

    let mut b = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, opts.cache_control)
        .header("x-content-type-options", "nosniff")
        // Even inline media gets an opaque origin if a browser renders it as a page.
        .header(header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; media-src 'self'; img-src 'self'; style-src 'unsafe-inline'")
        .header("x-tgv-object-id", &obj.id)
        .header("last-modified", httpdate(last_mod));

    if let Some(inm) = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) {
        if inm.split(',').any(|t| t.trim() == etag || t.trim() == "*") {
            return b.status(StatusCode::NOT_MODIFIED).body(Body::empty()).map_err(|e| ApiError::Internal(e.to_string()));
        }
    }

    let mut range = parse_range(headers.get(header::RANGE).and_then(|v| v.to_str().ok()), size)?;
    if let Some(ir) = headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok()) {
        if ir.trim() != etag {
            range = None; // representation changed: send it whole
        }
    }

    let active = is_active_type(&obj.content_type);
    let ct = if active && obj.content_type.is_empty() { "application/octet-stream" } else { obj.content_type.as_str() };
    b = b.header(header::CONTENT_TYPE, ct).header(header::CONTENT_DISPOSITION, disposition(&obj.key, opts.force_download || active));

    let (status, start, end) = match range {
        Some((s, e)) => {
            b = b.header(header::CONTENT_RANGE, format!("bytes {s}-{}/{size}", e - 1));
            (StatusCode::PARTIAL_CONTENT, s, e)
        }
        None => (StatusCode::OK, 0, size),
    };
    b = b.header(header::CONTENT_LENGTH, end - start).status(status);

    if method == Method::HEAD {
        return b.body(Body::empty()).map_err(|e| ApiError::Internal(e.to_string()));
    }
    let stream = state.engine.read_range(obj, start, end).await?;
    b.body(Body::from_stream(stream)).map_err(|e| ApiError::Internal(e.to_string()))
}

pub fn httpdate(ts: i64) -> String {
    // RFC 7231 IMF-fixdate without pulling in a date crate.
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let ts = ts.max(0);
    let days = ts / 86400;
    let secs = ts % 86400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[(days % 7) as usize],
        d,
        MONTHS[(m - 1) as usize],
        y,
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

// Howard Hinnant's days-from-civil inverse.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(Some("bytes=0-9"), 100).unwrap(), Some((0, 10)));
        assert_eq!(parse_range(Some("bytes=90-"), 100).unwrap(), Some((90, 100)));
        assert_eq!(parse_range(Some("bytes=-10"), 100).unwrap(), Some((90, 100)));
        assert_eq!(parse_range(Some("bytes=-500"), 100).unwrap(), Some((0, 100)));
        assert_eq!(parse_range(Some("bytes=50-5000"), 100).unwrap(), Some((50, 100)));
        assert!(parse_range(Some("bytes=100-"), 100).is_err());
        assert!(parse_range(Some("bytes=9-3"), 100).is_err());
        assert!(parse_range(Some("bytes=-0"), 100).is_err());
        assert!(parse_range(Some("bytes=0-0"), 0).is_err());
        assert!(parse_range(Some("bytes=abc"), 100).is_err());
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100).unwrap(), None);
        assert_eq!(parse_range(Some("items=0-1"), 100).unwrap(), None);
        assert_eq!(parse_range(None, 100).unwrap(), None);
    }

    #[test]
    fn dates() {
        assert_eq!(httpdate(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(httpdate(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
    }

    #[test]
    fn active_types() {
        assert!(is_active_type("text/html; charset=utf-8"));
        assert!(is_active_type("image/svg+xml"));
        assert!(is_active_type("application/atom+xml"));
        assert!(!is_active_type("video/mp4"));
        assert!(!is_active_type("image/png"));
    }
}
