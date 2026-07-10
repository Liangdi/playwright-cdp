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

/// A standalone HTTP client. Cheap to clone — it shares one underlying
/// `reqwest::Client` and a set of default headers.
///
/// Mirrors <https://playwright.dev/docs/api/class-apirequestcontext>.
#[derive(Clone)]
pub struct APIRequestContext {
    client: reqwest::Client,
    default_headers: Headers,
}

impl APIRequestContext {
    /// Create a new context with the given default headers (cloned into the
    /// context). A fresh `reqwest::Client` is built once and reused.
    pub fn new(default_headers: Headers) -> Self {
        let client = reqwest::Client::builder()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            default_headers,
        }
    }

    /// The default headers carried by this context.
    pub fn default_headers(&self) -> &Headers {
        &self.default_headers
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

    /// Build + send a request for `method`, applying options, then capture the
    /// full body so accessors are cheap.
    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        options: Option<APIRequestOptions>,
    ) -> Result<APIResponse> {
        let options = options.unwrap_or_default();
        let mut builder = self.client.request(method, url);

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

        // Per-request timeout.
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
