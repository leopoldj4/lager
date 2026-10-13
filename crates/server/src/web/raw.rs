//! Raw files: `/<repo>/-/raw/<rev>/<path>` answers a file's bytes, for
//! images in rendered Markdown and for downloads.
//!
//! Files come from repositories, so they are never served as HTML: images
//! keep their type, other text is `text/plain`, everything else a download.
//! `nosniff` and a sandboxing CSP keep scripts in SVG files or text files
//! from running on this origin.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;

use super::read::{self, ReadError};
use crate::http::AppState;

const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// The repository and `<rev>/<path>` of a raw address, still encoded.
pub fn parse(path: &str) -> Option<(&str, &str)> {
    let (name, rest) = path.strip_prefix('/')?.split_once("/-/raw/")?;
    worktree_core::config::validate_repo_name(name).ok()?;
    Some((name, rest))
}

pub async fn serve(s: &AppState, headers: &HeaderMap, name: &str, rest: &str) -> Response {
    let Some(web) = &s.web else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(rest) = percent_decode_str(rest).decode_utf8() else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let who = match web.identity(&s.directory, headers).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    // The blob's object ID is the ETag.
    let cached = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().trim_start_matches("W/").trim_matches('"'));
    s.registry.svc.metrics.inc("web_raw_requests");
    let file = match read::raw(s, &who, name, &rest, cached).await {
        Ok(f) => f,
        Err(e) => return read_error(e),
    };
    let etag = HeaderValue::from_str(&format!("\"{}\"", file.blob)).expect("hex");
    let common = [
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        ),
        (header::ETAG, etag),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ),
    ];
    let Some(data) = file.data else {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    };
    (
        common,
        [(header::CONTENT_TYPE, content_type(&file.path, &data))],
        data,
    )
        .into_response()
}

fn content_type(path: &str, data: &[u8]) -> &'static str {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ if !data.contains(&0) && std::str::from_utf8(data).is_ok() => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn read_error(e: ReadError) -> Response {
    let status = match e.code {
        "not_found" | "not_a_file" => StatusCode::NOT_FOUND,
        "bad_request" => StatusCode::BAD_REQUEST,
        "too_large" => StatusCode::FORBIDDEN,
        "busy" => StatusCode::TOO_MANY_REQUESTS,
        "unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, &e.message)
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-cache")],
        format!("{message}\n"),
    )
        .into_response()
}
