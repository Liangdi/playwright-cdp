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

/// Glob match supporting leading/trailing/both `*` (substring) and exact.
pub(crate) fn pattern_matches(pattern: &str, url: &str) -> bool {
    if pattern == url {
        return true;
    }
    if pattern.len() >= 2 && pattern.starts_with('*') && pattern.ends_with('*') {
        return url.contains(&pattern[1..pattern.len() - 1]);
    }
    if let Some(rest) = pattern.strip_prefix('*') {
        return url.ends_with(rest);
    }
    if let Some(rest) = pattern.strip_suffix('*') {
        return url.starts_with(rest);
    }
    url.contains(pattern)
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
