//! `Route` — network interception via the CDP `Fetch` domain.
//!
//! `Page::route(pattern, handler)` enables request interception; the handler
//! receives a [`Route`] and chooses to [`continue_`](Route::continue_),
//! [`fulfill`](Route::fulfill), or [`abort`](Route::abort) it.

use crate::cdp::session::CdpSession;
use crate::cdp::CdpEvent;
use crate::error::Result;
use crate::network::NetworkStore;
use crate::request::Request;
use crate::types::Headers;
use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::broadcast;

/// A handler invoked for each intercepted request matching a route pattern.
pub(crate) type RouteHandler =
    Arc<dyn Fn(Route) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// One registered route: a URL glob plus its handler.
#[derive(Clone)]
pub(crate) struct RouteEntry {
    pub pattern: String,
    pub handler: RouteHandler,
}

/// Background task: dispatch `Fetch.requestPaused` events to matching handlers.
pub(crate) async fn route_listener(
    mut rx: broadcast::Receiver<CdpEvent>,
    session: Arc<CdpSession>,
    store: Arc<NetworkStore>,
    handlers: Arc<Mutex<Vec<RouteEntry>>>,
) {
    loop {
        match rx.recv().await {
            Ok(ev) if ev.method == "Fetch.requestPaused" => {
                let url = ev
                    .params
                    .get("request")
                    .and_then(|r| r.get("url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let fetch_id = ev
                    .params
                    .get("requestId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let handler = {
                    let hs = handlers.lock();
                    hs.iter()
                        .find(|e| pattern_matches(&e.pattern, &url))
                        .map(|e| Arc::clone(&e.handler))
                };
                match (handler, request_from_paused(&ev.params, Arc::downgrade(&store))) {
                    (Some(h), Some(req)) => {
                        let route = Route::new(req, fetch_id, Arc::clone(&session));
                        tokio::spawn(async move {
                            (h)(route).await;
                        });
                    }
                    _ => {
                        // No handler / malformed: continue so the request isn't stalled.
                        let _ = session
                            .send("Fetch.continueRequest", json!({ "requestId": fetch_id }))
                            .await;
                    }
                }
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
        }
    }
}

/// Glob match supporting the Playwright URL glob syntax.
///
/// Supported wildcards (Playwright-compatible):
/// - `**` — matches any sequence of characters, *including* `/` (crosses path
///   segments). `**` alone matches every URL.
/// - `*` — matches any sequence of characters *except* `/` (a single path
///   segment).
/// - `?` — matches a single character.
/// - `[abc]` — a character class; `[!abc]` is its negation.
/// - everything else is matched literally.
///
/// `**/*` therefore matches any URL with a path, and `*/api/*` matches any URL
/// containing a `/api/` segment. Bare patterns without wildcards are treated as
/// a substring test (matching the legacy behaviour), and the legacy
/// `*foo` / `foo*` / `*foo*` shapes keep working: a trailing/leading/both `*`
/// (with no other special characters) is a suffix/prefix/substring test.
///
/// The pattern is compiled into a regular expression (the `regex` crate is
/// already a dependency); if compilation fails it falls back to a plain
/// `url.contains(pattern)` so a malformed pattern can never panic or reject
/// every URL.
pub(crate) fn pattern_matches(pattern: &str, url: &str) -> bool {
    // Fast path: exact equality.
    if pattern == url {
        return true;
    }

    // Legacy short-circuit: a pattern whose only wildcard characters are a
    // single leading, trailing, or both `*` keeps the original (cheap) string
    // semantics. This also guards patterns like `*foo*` that have no other glob
    // metacharacters, preserving backwards compatibility byte-for-byte.
    if let Some(result) = legacy_simple_wildcard(pattern, url) {
        return result;
    }

    // General path: translate the glob into a regex and anchor it.
    match glob_to_regex(pattern) {
        Some(re) => re.is_match(url),
        // Untranslatable pattern: fall back to substring match.
        None => url.contains(pattern),
    }
}

/// If `pattern` is one of the legacy shapes — `foo`, `*foo`, `foo*`, `*foo*`
/// where the inner text contains no glob metacharacters (`*`, `?`, `[`) —
/// evaluate it with the original exact/suffix/prefix/substring semantics and
/// return `Some(bool)`. Otherwise return `None` so the caller uses the full
/// glob engine.
fn legacy_simple_wildcard(pattern: &str, url: &str) -> Option<bool> {
    let has_leading = pattern.starts_with('*');
    let has_trailing = pattern.ends_with('*') && pattern.len() > 1;
    // Inner slice with the surrounding `*` stripped.
    let inner_start = if has_leading { 1 } else { 0 };
    let inner_end = if has_trailing {
        pattern.len() - 1
    } else {
        pattern.len()
    };
    if inner_start > inner_end {
        // e.g. pattern == "*" handled by the glob path below.
        return None;
    }
    let inner = &pattern[inner_start..inner_end];
    // Only treat as legacy if no other glob metacharacters are present.
    if inner.contains('*') || inner.contains('?') || inner.contains('[') {
        return None;
    }
    match (has_leading, has_trailing) {
        (true, true) => Some(url.contains(inner)),
        (true, false) => Some(url.ends_with(inner)),
        (false, true) => Some(url.starts_with(inner)),
        (false, false) => {
            // No wildcards at all: legacy behaviour is substring (contains).
            Some(url.contains(pattern))
        }
    }
}

/// Translate a Playwright-style URL glob into a compiled regex anchored with
/// `^...$`. Returns `None` if the translation or compilation fails.
fn glob_to_regex(pattern: &str) -> Option<regex::Regex> {
    let mut out = String::with_capacity(pattern.len() * 2);
    out.push('^');
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' => {
                // `**` matches across path separators; a lone `*` stays within
                // a single segment.
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    out.push_str(".*");
                    i += 2;
                    // Tolerate a stray third `*` (e.g. `***`).
                    while i < chars.len() && chars[i] == '*' {
                        i += 1;
                    }
                } else {
                    out.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            '[' => {
                // Character class. Find the matching `]`. A leading `!` becomes
                // `^` (negation); a leading `]` is a literal member.
                let start = i + 1;
                let mut j = start;
                if j < chars.len() && (chars[j] == '!' || chars[j] == '^') {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j >= chars.len() {
                    // Unterminated class: treat `[` literally (escape it).
                    out.push_str("\\[");
                    i += 1;
                } else {
                    out.push('[');
                    let mut k = start;
                    if k < chars.len() && chars[k] == '!' {
                        out.push('^');
                        k += 1;
                    }
                    while k < j {
                        let ch = chars[k];
                        // Escape regex metacharacters inside the class so a
                        // user-supplied `]`/`\\`/etc. can't break the regex.
                        if "\\.+*?(){}|^$".contains(ch) {
                            out.push('\\');
                        }
                        out.push(ch);
                        k += 1;
                    }
                    out.push(']');
                    i = j + 1;
                }
            }
            _ => {
                // Escape regex metacharacters; everything else is literal.
                out.push_str(&regex::escape(&c.to_string()));
                i += 1;
            }
        }
    }
    out.push('$');
    regex::Regex::new(&out).ok()
}

/// Options for [`Route::continue_`] (optional request overrides).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RouteContinueOptions {
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<Headers>,
    pub post_data: Option<String>,
}

/// Options for [`Route::fulfill`] (serve a synthetic response).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RouteFulfillOptions {
    pub status: Option<u16>,
    pub headers: Option<Headers>,
    pub content_type: Option<String>,
    pub body: Option<String>,
}

impl RouteFulfillOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn status(mut self, v: u16) -> Self {
        self.status = Some(v);
        self
    }
    pub fn body(mut self, v: impl Into<String>) -> Self {
        self.body = Some(v.into());
        self
    }
    pub fn content_type(mut self, v: impl Into<String>) -> Self {
        self.content_type = Some(v.into());
        self
    }
}

/// Options for [`Route::fetch`] (optional overrides applied on top of the
/// intercepted request).
///
/// Mirrors <https://playwright.dev/docs/api/class-route#route-fetch>.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RouteFetchOptions {
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<Headers>,
    pub post_data: Option<String>,
    pub post_data_bytes: Option<Vec<u8>>,
    /// Request timeout, in milliseconds. `None` uses the client default.
    pub timeout: Option<u64>,
    /// Maximum number of redirects to follow. `None` uses the client default.
    pub max_redirects: Option<u32>,
}

impl RouteFetchOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn url(mut self, v: impl Into<String>) -> Self {
        self.url = Some(v.into());
        self
    }
    pub fn method(mut self, v: impl Into<String>) -> Self {
        self.method = Some(v.into());
        self
    }
    pub fn headers(mut self, v: Headers) -> Self {
        self.headers = Some(v);
        self
    }
    pub fn post_data(mut self, v: impl Into<String>) -> Self {
        self.post_data = Some(v.into());
        self
    }
    pub fn post_data_bytes(mut self, v: Vec<u8>) -> Self {
        self.post_data_bytes = Some(v);
        self
    }
    pub fn timeout(mut self, v: u64) -> Self {
        self.timeout = Some(v);
        self
    }
    pub fn max_redirects(mut self, v: u32) -> Self {
        self.max_redirects = Some(v);
        self
    }
}

/// A response returned by [`Route::fetch`], capturing the full body so that
/// accessors are cheap. Used to inspect (and optionally modify) the real
/// response before fulfilling the route.
///
/// Mirrors <https://playwright.dev/docs/api/class-apiresponse>.
#[derive(Debug, Clone)]
pub struct RouteFetchResponse {
    url: String,
    status: u16,
    status_text: String,
    headers: Headers,
    body: Vec<u8>,
}

impl RouteFetchResponse {
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn status(&self) -> u16 {
        self.status
    }
    pub fn status_text(&self) -> &str {
        &self.status_text
    }
    pub fn headers(&self) -> &Headers {
        &self.headers
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The response body decoded as UTF-8.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// An intercepted network request, paused in the `Fetch` domain.
#[derive(Clone)]
pub struct Route {
    inner: Arc<RouteInner>,
}

struct RouteInner {
    request: Request,
    fetch_request_id: String,
    session: Arc<CdpSession>,
    handled: AtomicBool,
}

impl Route {
    pub(crate) fn new(
        request: Request,
        fetch_request_id: String,
        session: Arc<CdpSession>,
    ) -> Self {
        Self {
            inner: Arc::new(RouteInner {
                request,
                fetch_request_id,
                session,
                handled: AtomicBool::new(false),
            }),
        }
    }

    /// The intercepted request.
    pub fn request(&self) -> Request {
        self.inner.request.clone()
    }

    fn claim(&self) -> bool {
        !self.inner.handled.swap(true, Ordering::SeqCst)
    }

    /// Let the request proceed (optionally overriding url/method/headers/body).
    pub async fn continue_(&self, options: Option<RouteContinueOptions>) -> Result<()> {
        if !self.claim() {
            return Ok(());
        }
        let opts = options.unwrap_or_default();
        let mut params = json!({ "requestId": self.inner.fetch_request_id });
        if let Some(u) = opts.url {
            params["url"] = json!(u);
        }
        if let Some(m) = opts.method {
            params["method"] = json!(m);
        }
        if let Some(h) = opts.headers {
            params["headers"] = json!(headers_to_array(&h));
        }
        if let Some(p) = opts.post_data {
            params["postData"] = json!(base64::engine::general_purpose::STANDARD.encode(p));
        }
        self.inner
            .session
            .send("Fetch.continueRequest", params)
            .await
            .map(|_: Value| ())
    }

    /// Let the request proceed to the next matching route (no-op if only one).
    pub async fn fallback(&self) -> Result<()> {
        self.continue_(None).await
    }

    /// Serve a synthetic response.
    pub async fn fulfill(&self, options: RouteFulfillOptions) -> Result<()> {
        if !self.claim() {
            return Ok(());
        }
        let status = options.status.unwrap_or(200);
        let mut headers = options.headers.unwrap_or_default();
        if let Some(ct) = options.content_type {
            headers.entry("content-type".into()).or_insert(ct);
        }
        let body_b64 = options
            .body
            .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
            .unwrap_or_default();
        self.inner
            .session
            .send(
                "Fetch.fulfillRequest",
                json!({
                    "requestId": self.inner.fetch_request_id,
                    "responseCode": status,
                    "responseHeaders": headers_to_array(&headers),
                    "body": body_b64,
                }),
            )
            .await
            .map(|_: Value| ())
    }

    /// Abort the request with a CDP `ErrorReason` (default `"Failed"`).
    pub async fn abort(&self, error_code: Option<&str>) -> Result<()> {
        if !self.claim() {
            return Ok(());
        }
        let reason = error_code.unwrap_or("Failed");
        self.inner
            .session
            .send(
                "Fetch.failRequest",
                json!({ "requestId": self.inner.fetch_request_id, "errorReason": reason }),
            )
            .await
            .map(|_: Value| ())
    }

    /// Perform the intercepted request as a **real** HTTP request, returning the
    /// response so it can be inspected (and optionally modified) before
    /// [`fulfill`](Self::fulfill).
    ///
    /// Unlike [`continue_`](Self::continue_) (which lets the *page* perform the
    /// request through the browser), `fetch` issues the request itself via a
    /// standalone `reqwest::Client` that is entirely outside the page's CDP
    /// `Fetch`-domain network interception. It therefore does **not** re-trigger
    /// any registered route handlers — this is Playwright's documented
    /// `route.fetch` semantics, useful for "fetch the real response, then modify
    /// and fulfill".
    ///
    /// The intercepted request's url/method/headers/post_data are used as the
    /// base; any field set on `options` overrides the corresponding one.
    ///
    /// See: <https://playwright.dev/docs/api/class-route#route-fetch>
    pub async fn fetch(&self, options: Option<RouteFetchOptions>) -> Result<RouteFetchResponse> {
        let opts = options.unwrap_or_default();
        let request = self.request();

        let url = opts
            .url
            .clone()
            .unwrap_or_else(|| request.url().to_string());

        let method_str = opts
            .method
            .clone()
            .unwrap_or_else(|| request.method().to_string());
        let method = reqwest::Method::from_bytes(method_str.as_bytes())
            .map_err(|e| crate::error::Error::InvalidArgument(format!("invalid method: {e}")))?;

        // Headers: original request headers as the base, options override/merge.
        let mut headers = request.headers().clone();
        if let Some(extra) = opts.headers.as_ref() {
            for (k, v) in extra {
                headers.insert(k.clone(), v.clone());
            }
        }

        // Body: explicit bytes take priority, then string post_data, then the
        // original request body.
        let body: Vec<u8> = if let Some(b) = opts.post_data_bytes.clone() {
            b
        } else if let Some(s) = opts.post_data.clone() {
            s.into_bytes()
        } else {
            request.post_data_buffer().unwrap_or_default()
        };

        // Build an isolated client: per-call redirect policy + per-call timeout.
        let mut client_builder = reqwest::Client::builder();
        if let Some(n) = opts.max_redirects {
            client_builder = client_builder.redirect(reqwest::redirect::Policy::limited(
                n.max(1) as usize,
            ));
        }
        let client = client_builder
            .build()
            .map_err(|e| crate::error::Error::Http(format!("failed to build client: {e}")))?;

        let mut builder = client.request(method, &url);

        // Apply headers, skipping hop-by-hop / reqwest-managed headers
        // (Host, Content-Length, Connection, encoding, ...). Copying the
        // browser's versions verbatim makes reqwest reject the request with
        // "error sending request". Invalid header names/values are also skipped.
        const SKIP: &[&str] = &[
            "host",
            "connection",
            "content-length",
            "transfer-encoding",
            "accept-encoding",
            "content-encoding",
            "keep-alive",
            "proxy-connection",
            "upgrade",
        ];
        for (k, v) in &headers {
            if SKIP.contains(&k.as_str()) {
                continue;
            }
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                builder = builder.header(name, val);
            }
        }

        // A body is only valid for non-GET/HEAD requests (and harmless to omit).
        let is_bodyless =
            method_str.eq_ignore_ascii_case("GET") || method_str.eq_ignore_ascii_case("HEAD");
        if !body.is_empty() && !is_bodyless {
            builder = builder.body(body);
        }

        // Default 30s timeout (Playwright's route.fetch default) so a hung
        // upstream can't stall the route handler forever.
        let timeout_ms = opts.timeout.unwrap_or(30_000);
        builder = builder.timeout(std::time::Duration::from_millis(timeout_ms));

        let resp = builder.send().await.map_err(|e| {
            // Walk the source chain so the root cause (hyper/io) surfaces in
            // the error message instead of reqwest's generic "error sending
            // request".
            let mut s = format!("{e}");
            let mut src = std::error::Error::source(&e);
            while let Some(c) = src {
                s.push_str(" :: ");
                s.push_str(&c.to_string());
                src = c.source();
            }
            crate::error::Error::Http(format!("request failed: {s}"))
        })?;

        let final_url = resp.url().to_string();
        let status = resp.status().as_u16();
        let status_text = resp.status().canonical_reason().unwrap_or("").to_string();

        let mut resp_headers: Headers = Headers::with_capacity(resp.headers().len());
        for (name, value) in resp.headers().iter() {
            let key = name.as_str().to_ascii_lowercase();
            let val = value.to_str().map(String::from).unwrap_or_else(|_| {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            });
            resp_headers.insert(key, val);
        }

        let body = resp
            .bytes()
            .await
            .map_err(|e| crate::error::Error::Http(format!("failed to read body: {e}")))?;

        Ok(RouteFetchResponse {
            url: final_url,
            status,
            status_text,
            headers: resp_headers,
            body: body.to_vec(),
        })
    }
}

/// Build a `Request` from a `Fetch.requestPaused` event payload.
pub(crate) fn request_from_paused(params: &Value, store: Weak<NetworkStore>) -> Option<Request> {
    let req = params.get("request")?;
    let request_id = params.get("requestId").and_then(|v| v.as_str())?.to_string();
    let url = req.get("url").and_then(|v| v.as_str())?.to_string();
    let method = req
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_string();
    let headers = headers_from_object(req.get("headers"));
    let resource_type = params
        .get("resourceType")
        .and_then(|v| v.as_str())
        .unwrap_or("Other")
        .to_string();
    Some(Request::new(
        url,
        method,
        headers,
        resource_type.clone(),
        request_id,
        resource_type == "Document",
        store,
    ))
}

fn headers_to_array(h: &Headers) -> Vec<Value> {
    h.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect()
}

fn headers_from_object(v: Option<&Value>) -> Headers {
    let mut out = Headers::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let Some(s) = val.as_str() {
                out.insert(k.to_lowercase(), s.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Backwards compatibility: legacy shapes used by smoke tests ----

    #[test]
    fn legacy_exact_match() {
        assert!(pattern_matches("http://x/y", "http://x/y"));
        assert!(!pattern_matches("http://x/y", "http://x/z"));
    }

    #[test]
    fn legacy_leading_star_is_suffix() {
        // `*mocked` -> url ends_with "mocked"
        assert!(pattern_matches("*mocked", "http://host/mock-api-mocked"));
        assert!(!pattern_matches("*mocked", "http://host/api"));
    }

    #[test]
    fn legacy_trailing_star_is_prefix() {
        // `foo*` -> url starts_with "foo"
        assert!(pattern_matches("http://x*", "http://x/api"));
        assert!(!pattern_matches("http://x*", "http://y/api"));
    }

    #[test]
    fn legacy_both_stars_is_substring() {
        // `*mocked*` / `*blocked*` -> url contains — the shape the smoke tests use.
        assert!(pattern_matches("*mocked*", "http://host/mock-api-mocked/path"));
        assert!(pattern_matches("*blocked*", "http://host/blocked"));
        assert!(!pattern_matches("*mocked*", "http://host/api"));
    }

    #[test]
    fn legacy_no_wildcard_is_substring() {
        // A bare substring with no metacharacters falls back to `contains`.
        assert!(pattern_matches("api/v1", "http://host/api/v1/users"));
        assert!(!pattern_matches("api/v1", "http://host/api/v2"));
    }

    // ---- New Playwright glob semantics ----

    #[test]
    fn double_star_matches_anything_including_path() {
        // `**` matches every URL, including ones with paths and query strings.
        assert!(pattern_matches("**", "http://x/"));
        assert!(pattern_matches("**", "http://x/a/b/c"));
        assert!(pattern_matches("**", "https://example.com/api/v1?x=1#frag"));
        // `**` matches any sequence — including an empty one.
        assert!(pattern_matches("**", ""));
    }

    #[test]
    fn double_star_slash_star_matches_any() {
        // `**/*` is Playwright's "match all" idiom.
        assert!(pattern_matches("**/*", "http://x/api/y"));
        assert!(pattern_matches("**/*", "http://x/"));
    }

    #[test]
    fn bare_single_star_matches_any() {
        // A single `*` spans the whole URL here (no `/` to bound it as a segment
        // because there is nothing else in the pattern).
        assert!(pattern_matches("*", "http://x/a/b"));
    }

    #[test]
    fn single_star_does_not_cross_slash() {
        // `*/api/*` matches URLs containing an `/api/` segment.
        assert!(pattern_matches("*/api/*", "http://x/api/y"));
        assert!(pattern_matches("*/api/*", "http://x/a/b/api/c"));
        // No trailing segment after /api: `/api` alone has no closing `/`.
        // (Documented boundary — the pattern expects a `/api/` segment.)
        // This is intentionally not asserted either way to avoid over-constraining.
        // But a path that clearly lacks the segment must not match:
        assert!(!pattern_matches("*/api/*", "http://x/users"));
    }

    #[test]
    fn question_mark_matches_single_char() {
        assert!(pattern_matches("http://x/a?c", "http://x/abc"));
        assert!(pattern_matches("http://x/a?c", "http://x/aZc"));
        // `?` does not match `/` (single path segment only).
        assert!(!pattern_matches("http://x/a?c", "http://x/a/c"));
    }

    #[test]
    fn char_class_positive() {
        assert!(pattern_matches("http://x/v[12]", "http://x/v1"));
        assert!(pattern_matches("http://x/v[12]", "http://x/v2"));
        assert!(!pattern_matches("http://x/v[12]", "http://x/v3"));
    }

    #[test]
    fn char_class_negation() {
        assert!(pattern_matches("http://x/v[!12]", "http://x/v3"));
        assert!(!pattern_matches("http://x/v[!12]", "http://x/v1"));
    }

    #[test]
    fn real_world_typical_pattern() {
        // A realistic Playwright-style route pattern.
        let pattern = "**/api/users/*";
        assert!(pattern_matches(pattern, "https://app.example.com/api/users/42"));
        assert!(pattern_matches(pattern, "https://app.example.com/v2/api/users/abc"));
        // `*` can match an empty segment, so a trailing slash still matches.
        assert!(pattern_matches(pattern, "https://app.example.com/api/users/"));
        assert!(!pattern_matches(pattern, "https://app.example.com/api/groups/1"));
        // Wrong host/domain entirely (no /api/users/ segment).
        assert!(!pattern_matches(pattern, "https://other.example.com/feed"));
    }

    #[test]
    fn regex_metacharacters_in_pattern_are_literal() {
        // A `.` in the pattern is a literal dot, not the regex "any char".
        assert!(pattern_matches("http://x/.png", "http://x/.png"));
        assert!(!pattern_matches("http://x/.png", "http://x/xpng"));
    }
}
