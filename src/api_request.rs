//! Standalone HTTP client mirroring Playwright's `APIRequestContext` /
//! `APIResponse`.
//!
//! Unlike [`crate::Request`]/[`crate::Response`] (which represent network
//! traffic captured *inside* the browser page), an [`APIRequestContext`] is a
//! direct HTTP client — useful for exercising APIs alongside browser
//! automation. It carries a single shared `reqwest::Client` and a set of
//! default headers, and exposes the usual `get`/`post`/`put`/... verbs.
//!
//! Obtain one via [`crate::Page::request`] or [`crate::BrowserContext::request`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::options::APIRequestOptions;
use crate::types::Headers;

/// Configuration for building an [`APIRequestContext`].
///
/// Mirrors the relevant subset of Playwright's `newContext({ baseURL, ... })`
/// options that apply to a standalone API request context. Use [`Default`] for
/// sensible no-op defaults, then chain the builder setters:
///
/// ```no_run
/// # use std::time::Duration;
/// # use playwright_cdp::api_request::ApiRequestContextOptions;
/// let opts = ApiRequestContextOptions::default()
///     .base_url("https://api.example.com")
///     .user_agent("my-bot/1.0")
///     .timeout(Duration::from_secs(30))
///     .ignore_https_errors(true);
/// ```
///
/// See: <https://playwright.dev/docs/api/class-apirequestcontext>
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ApiRequestContextOptions {
    /// Base URL prepended to any relative request URL (one that does not start
    /// with `http://` or `https://`).
    pub base_url: Option<String>,
    /// `User-Agent` header applied to every request. Per-request headers take
    /// precedence.
    pub user_agent: Option<String>,
    /// Default per-request timeout. Overridden by a per-request `timeout` in
    /// [`APIRequestOptions`].
    pub timeout: Option<Duration>,
    /// When true, the underlying `reqwest::Client` accepts invalid TLS
    /// certificates (mirrors Playwright's `ignoreHTTPSErrors`).
    pub ignore_https_errors: bool,
    /// Extra headers merged into every request, below per-request headers.
    pub extra_http_headers: Headers,
}

impl ApiRequestContextOptions {
    /// Start from defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the base URL prepended to relative request URLs.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Set the `User-Agent` header applied to every request.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Set the default per-request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// When true, accept invalid TLS certificates.
    pub fn ignore_https_errors(mut self, ignore: bool) -> Self {
        self.ignore_https_errors = ignore;
        self
    }

    /// Set the extra headers merged into every request (replaces any
    /// previously set extras).
    pub fn extra_http_headers(mut self, headers: Headers) -> Self {
        self.extra_http_headers = headers;
        self
    }

    /// Add a single extra header.
    pub fn extra_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_http_headers.insert(name.into(), value.into());
        self
    }
}

/// A standalone HTTP client. Cheap to clone — it shares one underlying
/// `reqwest::Client` and a set of default headers.
///
/// Mirrors <https://playwright.dev/docs/api/class-apirequestcontext>.
#[derive(Clone)]
pub struct APIRequestContext {
    client: reqwest::Client,
    default_headers: Headers,
    options: ApiRequestContextOptions,
}

impl APIRequestContext {
    /// Create a new context with the given default headers (cloned into the
    /// context). A fresh `reqwest::Client` is built once and reused.
    ///
    /// Equivalent to [`APIRequestContext::new_with_options`] with the headers
    /// passed as `extra_http_headers` and all other options at their defaults.
    pub fn new(default_headers: Headers) -> Self {
        let mut options = ApiRequestContextOptions::default();
        options.extra_http_headers = default_headers;
        Self::new_with_options(options)
    }

    /// Create a new context from a full [`ApiRequestContextOptions`]. The
    /// underlying `reqwest::Client` is built once (honoring
    /// `ignore_https_errors` and the default `timeout`) and reused for every
    /// request.
    pub fn new_with_options(options: ApiRequestContextOptions) -> Self {
        let mut builder = reqwest::Client::builder();
        if options.ignore_https_errors {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(timeout) = options.timeout {
            builder = builder.timeout(timeout);
        }
        if let Some(user_agent) = options.user_agent.as_deref() {
            builder = builder.user_agent(user_agent);
        }
        let client = builder
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        // The default headers served to `default_headers()` are the
        // `extra_http_headers` from the options.
        let default_headers = options.extra_http_headers.clone();
        Self {
            client,
            default_headers,
            options,
        }
    }

    /// The default headers carried by this context (seeded from
    /// `extra_http_headers`).
    pub fn default_headers(&self) -> &Headers {
        &self.default_headers
    }

    /// The options this context was built with.
    pub fn options(&self) -> &ApiRequestContextOptions {
        &self.options
    }

    /// `GET`.
    pub async fn get(&self, url: &str, options: Option<APIRequestOptions>) -> Result<APIResponse> {
        self.send(reqwest::Method::GET, url, options).await
    }

    /// `POST`.
    pub async fn post(&self, url: &str, options: Option<APIRequestOptions>) -> Result<APIResponse> {
        self.send(reqwest::Method::POST, url, options).await
    }

    /// `PUT`.
    pub async fn put(&self, url: &str, options: Option<APIRequestOptions>) -> Result<APIResponse> {
        self.send(reqwest::Method::PUT, url, options).await
    }

    /// `PATCH`.
    pub async fn patch(&self, url: &str, options: Option<APIRequestOptions>) -> Result<APIResponse> {
        self.send(reqwest::Method::PATCH, url, options).await
    }

    /// `DELETE`.
    pub async fn delete(
        &self,
        url: &str,
        options: Option<APIRequestOptions>,
    ) -> Result<APIResponse> {
        self.send(reqwest::Method::DELETE, url, options).await
    }

    /// `HEAD`.
    pub async fn head(&self, url: &str, options: Option<APIRequestOptions>) -> Result<APIResponse> {
        self.send(reqwest::Method::HEAD, url, options).await
    }

    /// Resolve `url` against the configured base URL. Relative URLs (those not
    /// starting with `http://` or `https://`) are joined onto `base_url`;
    /// absolute URLs are returned verbatim. When no base URL is configured the
    /// input is returned unchanged.
    fn resolve_url(&self, url: &str) -> String {
        let Some(base) = self.options.base_url.as_deref() else {
            return url.to_string();
        };
        if url.starts_with("http://") || url.starts_with("https://") {
            return url.to_string();
        }
        // Join base + relative, handling the leading '/' on either side so we
        // avoid accidental double or missing slashes.
        let base = base.trim_end_matches('/');
        let path = if url.starts_with('/') {
            url.to_string()
        } else {
            format!("/{url}")
        };
        format!("{base}{path}")
    }

    /// Build + send a request for `method`, applying options, then capture the
    /// full body so accessors are cheap.
    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        options: Option<APIRequestOptions>,
    ) -> Result<APIResponse> {
        let options = options.unwrap_or_default();

        // Resolve a relative URL against the configured base URL. Absolute URLs
        // (and other schemes) are used verbatim.
        let url = self.resolve_url(url);

        let mut builder = self.client.request(method, &url);

        // The context-wide user_agent is already baked into the shared client
        // (set via ClientBuilder). Per-request headers below can still override
        // it by setting `User-Agent` explicitly.

        // Default headers first, then per-request overrides.
        for (k, v) in &self.default_headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        if let Some(headers) = options.headers.as_ref() {
            for (k, v) in headers {
                builder = builder.header(k.as_str(), v.as_str());
            }
        }

        // Query params.
        if let Some(params) = options.params.as_ref() {
            builder = builder.query(&params);
        }

        // Body: JSON `data` takes precedence over `form`.
        if let Some(data) = options.data.as_ref() {
            builder = builder.json(data);
        } else if let Some(form) = options.form.as_ref() {
            builder = builder.form(form);
        }

        // Per-request timeout overrides the context default.
        if let Some(timeout_ms) = options.timeout {
            builder = builder.timeout(Duration::from_millis(timeout_ms.max(0.0) as u64));
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| Error::Http(format!("request failed: {e}")))?;

        let url = resp.url().to_string();
        let status = resp.status().as_u16();
        // Lowercased header keys, matching the network Response shape.
        let mut headers: Headers = HashMap::with_capacity(resp.headers().len());
        for (name, value) in resp.headers().iter() {
            let key = name.as_str().to_ascii_lowercase();
            let val = match value.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => {
                    // Fallback: lossy decode of raw bytes (rare for HTTP headers).
                    String::from_utf8_lossy(value.as_bytes()).into_owned()
                }
            };
            headers.insert(key, val);
        }

        let body = resp
            .bytes()
            .await
            .map_err(|e| Error::Http(format!("failed to read body: {e}")))?;

        Ok(APIResponse {
            url,
            status,
            headers,
            body: Arc::from(body.as_ref()),
        })
    }
}

/// A captured HTTP response. The full body is read once at construction and
/// cached, so [`APIResponse::body`], [`APIResponse::text`], and
/// [`APIResponse::json`] are cheap and re-entrant.
///
/// Mirrors <https://playwright.dev/docs/api/class-apiresponse>.
#[derive(Debug, Clone)]
pub struct APIResponse {
    url: String,
    status: u16,
    headers: Headers,
    body: Arc<[u8]>,
}

impl APIResponse {
    /// The final URL after any redirects.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// True when the status is in the `200..300` range.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Response headers, with lowercased keys.
    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// The raw response body (cached).
    pub async fn body(&self) -> Result<Vec<u8>> {
        Ok(self.body.to_vec())
    }

    /// The response body decoded as UTF-8 (cached).
    pub async fn text(&self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.body).into_owned())
    }

    /// The response body parsed as JSON (re-parsed each call from the cached bytes).
    pub async fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.body).map_err(Into::into)
    }
}
