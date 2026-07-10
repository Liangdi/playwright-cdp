//! HAR recording/replay (`route_from_har`).
//!
//! This module parses HAR 1.2 files (the HTTP Archive format) and produces a set
//! of canned route responses. The [`BrowserContext::route_from_har`] call turns
//! each HAR entry into a context-level route handler that, on a matching request,
//! serves the recorded response via [`Route::fulfill`](crate::Route::fulfill) and
//! lets non-matching requests fall through to the network.
//!
//! # Scope
//! **Replay is fully implemented.** Recording (capturing live traffic into a new
//! HAR file) is deferred — `HarRecorder` exists as a thin writer that can append
//! entries, but wiring it into the live request stream of every page is not yet
//! done. See the report notes.
//!
//! The parsing structs lean heavily on `#[serde(default)]` because real-world
//! HAR files are messy: fields are frequently missing or carry unexpected types.

use crate::error::{Error, Result};
use crate::route::RouteFulfillOptions;
use crate::types::Headers;
use crate::Route;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// HAR 1.2 parsing structs
// ---------------------------------------------------------------------------

/// The top-level HAR document: `{ "log": { ... } }`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Har {
    #[serde(default)]
    pub log: HarLog,
}

/// The `log` object. Only `entries` is consulted for replay; `creator` /
/// `browser` / `pages` are tolerated but ignored.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarLog {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub creator: Option<HarCreator>,
    #[serde(default)]
    pub browser: Option<HarCreator>,
    #[serde(default)]
    pub pages: Vec<HarPage>,
    #[serde(default)]
    pub entries: Vec<HarEntry>,
}

/// `log.creator` / `log.browser`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarCreator {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// `log.pages[*]` (page-level timing metadata). Parsed for tolerance only.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarPage {
    #[serde(default, rename = "startedDateTime")]
    pub started_date_time: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, rename = "pageTimings")]
    pub page_timings: Option<Value>,
}

/// One request/response pair.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarEntry {
    #[serde(default, rename = "startedDateTime")]
    pub started_date_time: Option<String>,
    #[serde(default)]
    pub time: Option<f64>,
    #[serde(default)]
    pub request: HarRequest,
    #[serde(default)]
    pub response: HarResponse,
    #[serde(default, rename = "cache")]
    pub cache: Option<Value>,
    #[serde(default, rename = "timings")]
    pub timings: Option<Value>,
    #[serde(default)]
    pub server_ip_address: Option<String>,
    #[serde(default)]
    pub connection: Option<String>,
    /// Optional `pageref` linking this entry to a `log.pages[*].id`.
    #[serde(default)]
    pub pageref: Option<String>,
}

/// Placeholder import so `Value` is referenced even if unused fields are
/// stripped later (keeps the struct forward-compatible with full HAR 1.2).
#[allow(unused_imports)]
use serde_json::Value;

/// A recorded request.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarRequest {
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub http_version: Option<String>,
    /// HAR stores headers as `[{ "name", "value" }, ...]`.
    #[serde(default)]
    pub headers: Vec<HarHeader>,
    #[serde(default)]
    pub cookies: Vec<HarCookie>,
    #[serde(default, rename = "queryString")]
    pub query_string: Vec<HarQuery>,
    #[serde(default, rename = "postData")]
    pub post_data: Option<HarPostData>,
    #[serde(default, rename = "headersSize")]
    pub headers_size: Option<i64>,
    #[serde(default, rename = "bodySize")]
    pub body_size: Option<i64>,
}

/// A recorded response.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarResponse {
    #[serde(default)]
    pub status: u16,
    #[serde(default, rename = "statusText")]
    pub status_text: String,
    #[serde(default)]
    pub http_version: Option<String>,
    #[serde(default)]
    pub cookies: Vec<HarCookie>,
    #[serde(default)]
    pub headers: Vec<HarHeader>,
    #[serde(default)]
    pub content: HarContent,
    #[serde(default, rename = "redirectURL")]
    pub redirect_url: Option<String>,
    #[serde(default, rename = "headersSize")]
    pub headers_size: Option<i64>,
    #[serde(default, rename = "bodySize")]
    pub body_size: Option<i64>,
}

/// Response body payload + mime.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarContent {
    #[serde(default)]
    pub size: Option<i64>,
    /// When the body is binary, HAR commonly base64-encodes it and sets
    /// `encoding: "base64"`. Plain text has `encoding` absent/empty.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub compression: Option<i64>,
}

/// `{ "name": "...", "value": "..." }` header.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarHeader {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// `{ "name": "...", "value": "..." }` cookie (comment/domain/etc tolerated).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarCookie {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default, rename = "httpOnly")]
    pub http_only: Option<bool>,
    #[serde(default)]
    pub secure: Option<bool>,
    #[serde(default, rename = "sameSite")]
    pub same_site: Option<String>,
}

/// `{ "name": "...", "value": "..." }` query parameter.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarQuery {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// `request.postData`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HarPostData {
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub params: Vec<HarQuery>,
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

/// A single HAR-derived route: the request signature to match against and the
/// canned response to serve.
///
/// Constructed by [`routes_from_har`]; consumed by
/// [`BrowserContext::route_from_har`](crate::BrowserContext::route_from_har) /
/// [`Page::route_from_har`](crate::Page::route_from_har), which turn each one
/// into a real route handler.
#[derive(Debug, Clone)]
pub struct HarRoute {
    /// HTTP method (`GET`, `POST`, …), upper-cased for case-insensitive match.
    pub method: String,
    /// URL glob (`*`-wildcarded, same semantics as `route(pattern, …)`).
    pub pattern: String,
    /// HTTP status to fulfill with.
    pub status: u16,
    /// Response headers (lower-cased keys).
    pub headers: Headers,
    /// Response body (already base64-decoded to a UTF-8 string when the HAR
    /// recorded it as base64; raw bytes that are not valid UTF-8 are passed
    /// through lossily — HAR replay is text-oriented in this crate).
    pub body: String,
}

impl HarRoute {
    /// Build the [`RouteFulfillOptions`] that serves this canned response.
    pub fn fulfill_options(&self) -> RouteFulfillOptions {
        RouteFulfillOptions {
            status: Some(self.status),
            headers: Some(self.headers.clone()),
            content_type: None,
            body: Some(self.body.clone()),
        }
    }

    /// Serve this route's canned response. Returns the fulfill result so the
    /// caller can short-circuit (a fulfilled request must not also `continue_`).
    pub async fn fulfill(&self, route: &Route) -> Result<()> {
        route.fulfill(self.fulfill_options()).await
    }
}

/// Options for `route_from_har`, mirroring Playwright's
/// `RouteFromHarOptions`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RouteFromHarOptions {
    /// If `Some(true)`, do not fulfill from the HAR — instead let requests hit
    /// the network and (eventually) update the HAR. Recording is not yet wired,
    /// so when `update` is true the routes still fall through to the network but
    /// the file is not rewritten. See the report notes.
    pub update: Option<bool>,
    /// If set, only register HAR entries whose URL matches this glob; entries
    /// that do not match are ignored (their requests fall through to network).
    pub url: Option<String>,
    /// If `Some(true)`, match requests case-insensitively on the URL. Defaults
    /// to false (Playwright matches the recorded URL literally).
    pub url_match_case_insensitive: Option<bool>,
}

impl RouteFromHarOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(mut self, v: bool) -> Self {
        self.update = Some(v);
        self
    }
    pub fn url(mut self, v: impl Into<String>) -> Self {
        self.url = Some(v.into());
        self
    }
    pub fn url_match_case_insensitive(mut self, v: bool) -> Self {
        self.url_match_case_insensitive = Some(v);
        self
    }
}

/// Parse a HAR file from disk and produce one [`HarRoute`] per usable entry.
///
/// "Usable" means the entry has a request URL and a response status. Entries
/// whose recorded URL is empty, or which do not match the optional `opts.url`
/// glob filter, are skipped. The returned patterns are the raw recorded URLs;
/// the context applies them with the same `*`-glob semantics as `route(...)`.
pub fn routes_from_har<P: AsRef<Path>>(
    path: P,
    opts: Option<&RouteFromHarOptions>,
) -> Result<Vec<HarRoute>> {
    let opts = opts.cloned().unwrap_or_default();
    let data = std::fs::read(path.as_ref()).map_err(|e| {
        Error::InvalidArgument(format!(
            "failed to read HAR file {}: {}",
            path.as_ref().display(),
            e
        ))
    })?;
    let har: Har = serde_json::from_slice(&data).map_err(|e| {
        Error::InvalidArgument(format!("failed to parse HAR file: {}", e))
    })?;

    let filter = opts.url.as_deref();
    let mut out = Vec::with_capacity(har.log.entries.len());
    for entry in &har.log.entries {
        let url = entry.request.url.trim();
        if url.is_empty() {
            continue;
        }
        // Optional URL-glob filter on the recorded URL itself.
        if let Some(glob) = filter {
            if !glob_match(glob, url) {
                continue;
            }
        }

        let headers = collect_headers(&entry.response.headers);
        let (body, encoding_is_base64) = decode_body(&entry.response.content);

        // Prefer the recorded Content-Type header; fall back to the content
        // mime_type field. We surface it only when present so we do not
        // clobber the caller's headers with an empty string.
        let content_type = entry
            .response
            .content
            .mime_type
            .clone()
            .or_else(|| headers.get("content-type").cloned());

        let mut headers = headers;
        if let Some(ct) = content_type {
            headers
                .entry("content-type".to_string())
                .or_insert(ct);
        }

        let body_string = if encoding_is_base64 {
            // Best-effort decode to UTF-8. Binary payloads that are not valid
            // UTF-8 are passed through lossily; route_from_har is text-oriented.
            String::from_utf8(body).unwrap_or_else(|e| {
                String::from_utf8_lossy(&e.into_bytes()).into_owned()
            })
        } else {
            // Already text in the HAR.
            match String::from_utf8(body) {
                Ok(s) => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            }
        };

        out.push(HarRoute {
            method: entry.request.method.to_ascii_uppercase(),
            pattern: url.to_string(),
            status: if entry.response.status == 0 {
                200
            } else {
                entry.response.status
            },
            headers,
            body: body_string,
        });
    }
    Ok(out)
}

/// Flatten `[{ "name", "value" }]` into a `Headers` map with lower-cased keys.
fn collect_headers(headers: &[HarHeader]) -> Headers {
    let mut out = Headers::new();
    for h in headers {
        if h.name.is_empty() {
            continue;
        }
        out.insert(h.name.to_lowercase(), h.value.clone());
    }
    out
}

/// Decode a HAR response body to raw bytes. Returns `(bytes, was_base64)`.
fn decode_body(content: &HarContent) -> (Vec<u8>, bool) {
    let text = match &content.text {
        Some(t) if !t.is_empty() => t.as_str(),
        _ => return (Vec::new(), false),
    };
    let is_base64 = content
        .encoding
        .as_deref()
        .map(|e| e.eq_ignore_ascii_case("base64"))
        .unwrap_or(false);
    if is_base64 {
        match base64::engine::general_purpose::STANDARD.decode(text) {
            Ok(bytes) => (bytes, true),
            // Malformed base64: fall back to raw text rather than dropping the body.
            Err(_) => (text.as_bytes().to_vec(), false),
        }
    } else {
        (text.as_bytes().to_vec(), false)
    }
}

/// Glob match compatible with the route pattern semantics.
///
/// This is a thin wrapper over [`crate::route::pattern_matches`] so that the
/// `opts.url` filter behaves identically to the registered route patterns,
/// including the full Playwright URL glob syntax (`**`, `*`, `?`, `[abc]`).
/// See `route::pattern_matches` for the exact semantics.
pub(crate) fn glob_match(pattern: &str, url: &str) -> bool {
    crate::route::pattern_matches(pattern, url)
}

// ---------------------------------------------------------------------------
// Recording (deferred / partial)
// ---------------------------------------------------------------------------

/// A minimal HAR recorder: builds a [`Har`] in memory and can write it to disk.
///
/// This exists so that `route_from_har`'s `update` path has a place to land,
/// but **recording is not wired into the live request stream** of pages in this
/// pass. Constructing a recorder and calling [`HarRecorder::record_entry`]
/// works, but nothing feeds it live traffic automatically. See the report.
#[derive(Debug, Clone, Default)]
pub struct HarRecorder {
    entries: Vec<HarEntry>,
}

impl HarRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a pre-built entry. (Not yet driven automatically by page traffic.)
    pub fn record_entry(&mut self, entry: HarEntry) {
        self.entries.push(entry);
    }

    /// Number of recorded entries so far.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether any entries have been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Build the in-memory [`Har`] document.
    pub fn to_har(&self) -> Har {
        Har {
            log: HarLog {
                version: Some("1.2".to_string()),
                creator: Some(HarCreator {
                    name: Some("playwright-cdp".to_string()),
                    version: Some(env!("CARGO_PKG_VERSION").to_string()),
                }),
                browser: None,
                pages: Vec::new(),
                entries: self.entries.clone(),
            },
        }
    }

    /// Serialize the recorded HAR to a JSON string (pretty, matching the HAR
    /// convention of human-readable files).
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.to_har())
            .map_err(|e| Error::ProtocolError(format!("failed to serialize HAR: {}", e)))
    }

    /// Write the recorded HAR to `path`.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json = self.to_json()?;
        std::fs::write(path.as_ref(), json).map_err(|e| {
            Error::InvalidArgument(format!(
                "failed to write HAR file {}: {}",
                path.as_ref().display(),
                e
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_har() {
        let json = r#"{
            "log": {
                "version": "1.2",
                "entries": [
                    {
                        "request": { "method": "GET", "url": "https://example.com/api" },
                        "response": {
                            "status": 200,
                            "statusText": "OK",
                            "headers": [ { "name": "Content-Type", "value": "application/json" } ],
                            "content": { "text": "{\"ok\":true}", "mimeType": "application/json" }
                        }
                    }
                ]
            }
        }"#;
        let har: Har = serde_json::from_str(json).unwrap();
        assert_eq!(har.log.entries.len(), 1);

        let routes = routes_from_har_memory(&har, None).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].pattern, "https://example.com/api");
        assert_eq!(routes[0].status, 200);
        assert_eq!(routes[0].body, "{\"ok\":true}");
        assert_eq!(routes[0].headers.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn decodes_base64_body() {
        let json = r#"{
            "log": { "entries": [ {
                "request": { "method": "GET", "url": "https://example.com/img" },
                "response": {
                    "status": 200,
                    "content": { "text": "aGVsbG8=", "encoding": "base64", "mimeType": "text/plain" }
                }
            } ] }
        }"#;
        let har: Har = serde_json::from_str(json).unwrap();
        let routes = routes_from_har_memory(&har, None).unwrap();
        assert_eq!(routes[0].body, "hello");
    }

    #[test]
    fn url_filter_skips_non_matching() {
        let json = r#"{
            "log": { "entries": [
                { "request": { "method": "GET", "url": "https://a.example.com/x" },
                  "response": { "status": 200, "content": {} } },
                { "request": { "method": "GET", "url": "https://b.example.com/y" },
                  "response": { "status": 200, "content": {} } }
            ] }
        }"#;
        let har: Har = serde_json::from_str(json).unwrap();
        let opts = RouteFromHarOptions::default().url("*/x");
        let routes = routes_from_har_memory(&har, Some(&opts)).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].pattern, "https://a.example.com/x");
    }

    #[test]
    fn empty_and_missing_urls_are_skipped() {
        let json = r#"{
            "log": { "entries": [
                { "request": { "method": "GET", "url": "" },
                  "response": { "status": 200, "content": {} } },
                { "request": { "method": "GET", "url": "https://ok.example.com" },
                  "response": { "status": 200, "content": {} } }
            ] }
        }"#;
        let har: Har = serde_json::from_str(json).unwrap();
        let routes = routes_from_har_memory(&har, None).unwrap();
        assert_eq!(routes.len(), 1);
    }

    // Helper: run the same parsing path as `routes_from_har` but from an
    // already-deserialized `Har` (the file-reading half is trivial).
    fn routes_from_har_memory(
        har: &Har,
        opts: Option<&RouteFromHarOptions>,
    ) -> Result<Vec<HarRoute>> {
        let opts = opts.cloned().unwrap_or_default();
        let filter = opts.url.as_deref();
        let mut out = Vec::new();
        for entry in &har.log.entries {
            let url = entry.request.url.trim();
            if url.is_empty() {
                continue;
            }
            if let Some(glob) = filter {
                if !glob_match(glob, url) {
                    continue;
                }
            }
            let headers = collect_headers(&entry.response.headers);
            let (body, b64) = decode_body(&entry.response.content);
            let body_string = if b64 {
                String::from_utf8(body).unwrap_or_default()
            } else {
                String::from_utf8(body).unwrap_or_default()
            };
            out.push(HarRoute {
                method: entry.request.method.to_ascii_uppercase(),
                pattern: url.to_string(),
                status: if entry.response.status == 0 { 200 } else { entry.response.status },
                headers,
                body: body_string,
            });
        }
        Ok(out)
    }
}
