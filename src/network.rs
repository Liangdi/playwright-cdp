//! Network capture: stores HTTP requests/responses observed via the CDP
//! `Network` domain and drives `Page::on_request` / `on_response` handlers.
//!
//! Beyond the basic `Request` / `Response` wrappers, the store also keeps a
//! side table of extra CDP-captured detail per `requestId` (post data, redirect
//! chain, failure text, timing/sizes, security details, remote address, …)
//! which the wrappers surface through their accessor methods.

use crate::cdp::session::CdpSession;
use crate::cdp::CdpEvent;
use crate::request::Request;
use crate::response::Response;
use crate::types::Headers;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::broadcast;

pub(crate) type RequestHandler =
    Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;
pub(crate) type ResponseHandler =
    Arc<dyn Fn(Response) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

// ---------------------------------------------------------------------------
// Detail value types. These live here (in the store) so both `Request` and
// `Response` can reuse them without a cross-dependency.
// ---------------------------------------------------------------------------

/// Timing information for a request/response, derived from the CDP
/// `Network.Response.timing` object. All times are Unix epoch milliseconds
/// where applicable; durations are milliseconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timing {
    /// Request start time (ms since epoch).
    pub request_time: Option<f64>,
    /// Time spent in proxy handling (ms).
    pub proxy_start: Option<f64>,
    /// Time spent until proxy finished (ms, relative offset from `proxy_start`).
    pub proxy_end: Option<f64>,
    /// DNS resolution duration (ms).
    pub dns_start: Option<f64>,
    pub dns_end: Option<f64>,
    /// Connection duration (ms).
    pub connect_start: Option<f64>,
    pub connect_end: Option<f64>,
    /// TLS/SSL duration (ms).
    pub ssl_start: Option<f64>,
    pub ssl_end: Option<f64>,
    /// Worker start (ms).
    pub worker_start: Option<f64>,
    pub worker_ready: Option<f64>,
    pub worker_fetch_start: Option<f64>,
    /// Time until response headers received (ms).
    pub send_start: Option<f64>,
    pub send_end: Option<f64>,
    /// Time until the server started responding (ms).
    pub receive_headers_end: Option<f64>,
}

impl Timing {
    fn from_value(v: Option<&Value>) -> Option<Self> {
        let t = v?;
        Some(Timing {
            request_time: t.get("requestTime").and_then(|v| v.as_f64()),
            proxy_start: t.get("proxyStart").and_then(|v| v.as_f64()),
            proxy_end: t.get("proxyEnd").and_then(|v| v.as_f64()),
            dns_start: t.get("dnsStart").and_then(|v| v.as_f64()),
            dns_end: t.get("dnsEnd").and_then(|v| v.as_f64()),
            connect_start: t.get("connectStart").and_then(|v| v.as_f64()),
            connect_end: t.get("connectEnd").and_then(|v| v.as_f64()),
            ssl_start: t.get("sslStart").and_then(|v| v.as_f64()),
            ssl_end: t.get("sslEnd").and_then(|v| v.as_f64()),
            worker_start: t.get("workerStart").and_then(|v| v.as_f64()),
            worker_ready: t.get("workerReady").and_then(|v| v.as_f64()),
            worker_fetch_start: t.get("workerFetchStart").and_then(|v| v.as_f64()),
            send_start: t.get("sendStart").and_then(|v| v.as_f64()),
            send_end: t.get("sendEnd").and_then(|v| v.as_f64()),
            receive_headers_end: t
                .get("receiveHeadersEnd")
                .and_then(|v| v.as_f64()),
        })
    }
}

/// Size breakdown for a transferred resource, derived from CDP
/// `Network.loadingFinished` (`encodedDataLength`) and the response headers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sizes {
    /// Total bytes transferred on the wire (encoded, as reported by CDP
    /// `Network.loadingFinished.encodedDataLength`).
    pub encoded_data_length: Option<u64>,
}

/// TLS/SSL certificate details, derived from the CDP
/// `Network.Response.securityDetails` object.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityDetails {
    /// TLS protocol name (e.g. "TLS 1.3").
    pub protocol: Option<String>,
    /// Certificate subject (CN).
    pub subject_name: Option<String>,
    /// Certificate issuer (CN).
    pub issuer: Option<String>,
    /// Certificate validity start (Unix epoch seconds).
    pub valid_from: Option<f64>,
    /// Certificate validity end (Unix epoch seconds).
    pub valid_to: Option<f64>,
}

impl SecurityDetails {
    fn from_value(v: Option<&Value>) -> Option<Self> {
        let s = v?;
        Some(SecurityDetails {
            protocol: s.get("protocol").and_then(|v| v.as_str()).map(String::from),
            subject_name: s
                .get("subjectName")
                .and_then(|v| v.as_str())
                .map(String::from),
            issuer: s.get("issuer").and_then(|v| v.as_str()).map(String::from),
            valid_from: s.get("validFrom").and_then(|v| v.as_f64()),
            valid_to: s.get("validTo").and_then(|v| v.as_f64()),
        })
    }
}

/// The remote server address a response came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerAddr {
    /// The IP literal (e.g. "93.184.216.34").
    pub ip_address: Option<String>,
    /// The remote port.
    pub port: Option<u16>,
}

impl ServerAddr {
    fn from_value(v: Option<&Value>) -> Option<Self> {
        let r = v?;
        let ip_address = r.get("remoteIPAddress").and_then(|v| v.as_str()).map(String::from);
        let port = r
            .get("remotePort")
            .and_then(|v| v.as_i64())
            .map(|n| n as u16);
        // Only report when at least the address was present.
        if ip_address.is_none() && port.is_none() {
            None
        } else {
            Some(ServerAddr { ip_address, port })
        }
    }
}

/// The full set of extra detail captured for a single `requestId`.
#[derive(Debug, Clone, Default)]
struct RequestDetail {
    // --- request side ---
    /// Raw request body (`Network.requestWillBeSent.postData`).
    post_data: Option<String>,
    /// CDP `requestId` of the request this one redirected from
    /// (`Network.requestWillBeSent.redirectSource`), if any.
    redirected_from: Option<String>,
    /// CDP `requestId` of the request this one redirected to
    /// (the inverse link, populated when a redirect is observed).
    redirected_to: Option<String>,
    /// Failure text (`Network.loadingFailed.errorText`), if it failed.
    failure: Option<String>,
    // --- response / loading side ---
    /// Response timing (`Network.Response.timing`).
    timing: Option<Timing>,
    /// Wire size (`Network.loadingFinished.encodedDataLength`).
    sizes: Option<Sizes>,
    /// HTTP/protocol version (`Network.Response.protocol`), e.g. "http/1.1".
    http_version: Option<String>,
    /// Security details (`Network.Response.securityDetails`).
    security_details: Option<SecurityDetails>,
    /// Remote server address (`Network.Response.remoteIPAddress/Port`).
    server_addr: Option<ServerAddr>,
    /// Raw response headers (original case, from `Network.Response.headers`).
    raw_response_headers: Option<Headers>,
    /// Raw request headers (original case, from the request object).
    raw_request_headers: Option<Headers>,
    /// `true` once `Network.loadingFinished` was observed.
    finished: bool,
}

/// Per-page store of observed requests/responses, keyed by CDP `requestId`.
pub(crate) struct NetworkStore {
    self_ref: Mutex<Weak<NetworkStore>>,
    requests: Mutex<HashMap<String, Request>>,
    responses: Mutex<HashMap<String, Response>>,
    /// Extra captured detail per `requestId`.
    details: Mutex<HashMap<String, RequestDetail>>,
    /// Document-request `loaderId` → `requestId` (for correlating navigations).
    loaders: Mutex<HashMap<String, String>>,
    /// Count of requests sent but not yet finished (for `networkidle`).
    inflight: AtomicU64,
}

impl NetworkStore {
    pub(crate) fn new() -> Arc<Self> {
        let s = Arc::new(Self {
            self_ref: Mutex::new(Weak::new()),
            requests: Mutex::new(HashMap::new()),
            responses: Mutex::new(HashMap::new()),
            details: Mutex::new(HashMap::new()),
            loaders: Mutex::new(HashMap::new()),
            inflight: AtomicU64::new(0),
        });
        *s.self_ref.lock() = Arc::downgrade(&s);
        s
    }

    /// Current in-flight request count.
    pub(crate) fn inflight(&self) -> u64 {
        self.inflight.load(Ordering::Relaxed)
    }

    fn weak(&self) -> Weak<NetworkStore> {
        self.self_ref.lock().clone()
    }

    pub(crate) fn get_request(&self, request_id: &str) -> Option<Request> {
        self.requests.lock().get(request_id).cloned()
    }

    pub(crate) fn get_response(&self, request_id: &str) -> Option<Response> {
        self.responses.lock().get(request_id).cloned()
    }

    // --- detail accessors used by Request / Response ---

    pub(crate) fn post_data(&self, request_id: &str) -> Option<String> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.post_data.clone())
    }

    pub(crate) fn redirect_source(&self, request_id: &str) -> Option<String> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.redirected_from.clone())
    }

    pub(crate) fn redirect_target(&self, request_id: &str) -> Option<String> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.redirected_to.clone())
    }

    pub(crate) fn failure(&self, request_id: &str) -> Option<String> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.failure.clone())
    }

    pub(crate) fn timing(&self, request_id: &str) -> Option<Timing> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.timing.clone())
    }

    pub(crate) fn sizes(&self, request_id: &str) -> Option<Sizes> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.sizes.clone())
    }

    pub(crate) fn http_version(&self, request_id: &str) -> Option<String> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.http_version.clone())
    }

    pub(crate) fn security_details(&self, request_id: &str) -> Option<SecurityDetails> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.security_details.clone())
    }

    pub(crate) fn server_addr(&self, request_id: &str) -> Option<ServerAddr> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.server_addr.clone())
    }

    pub(crate) fn raw_response_headers(&self, request_id: &str) -> Option<Headers> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.raw_response_headers.clone())
    }

    pub(crate) fn raw_request_headers(&self, request_id: &str) -> Option<Headers> {
        self.details
            .lock()
            .get(request_id)
            .and_then(|d| d.raw_request_headers.clone())
    }

    pub(crate) fn finished(&self, request_id: &str) -> bool {
        self.details
            .lock()
            .get(request_id)
            .map(|d| d.finished)
            .unwrap_or(false)
    }

    /// The document response for a navigation `loaderId` (for `goto`).
    pub(crate) fn response_for_loader(&self, loader_id: &str) -> Option<Response> {
        let req_id = self.loaders.lock().get(loader_id).cloned()?;
        self.get_response(&req_id)
    }

    /// Handle `Network.requestWillBeSent`. Stores + returns the request.
    pub(crate) fn on_request_will_be_sent(&self, params: &Value) -> Option<Request> {
        let req = params.get("request")?;
        let request_id = params.get("requestId").and_then(|v| v.as_str())?.to_string();
        let url = req.get("url").and_then(|v| v.as_str())?.to_string();
        let method = req
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_string();
        let headers = headers_from_value(req.get("headers"));
        let resource_type = params
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("Other")
            .to_string();
        let is_navigation = resource_type == "Document";
        let loader_id = params.get("loaderId").and_then(|v| v.as_str()).map(String::from);

        let request = Request::new(
            url,
            method,
            headers,
            resource_type,
            request_id.clone(),
            is_navigation,
            self.weak(),
        );
        if is_navigation {
            if let Some(lid) = &loader_id {
                self.loaders.lock().insert(lid.clone(), request_id.clone());
            }
        }
        self.requests.lock().insert(request_id.clone(), request.clone());
        self.inflight.fetch_add(1, Ordering::Relaxed);

        // Capture extra request detail.
        let post_data = req
            .get("postData")
            .and_then(|v| v.as_str())
            .map(String::from);
        let raw_request_headers = headers_raw_from_value(req.get("headers"));
        // `redirectSource` carries the CDP requestId of the request that
        // redirected into this one.
        let redirected_from = params
            .get("redirectSource")
            .and_then(|v| v.as_str())
            .map(String::from);

        let mut details = self.details.lock();
        let entry = details.entry(request_id.clone()).or_default();
        entry.post_data = post_data;
        entry.raw_request_headers = raw_request_headers;
        if let Some(src) = &redirected_from {
            entry.redirected_from = Some(src.clone());
            // Back-link: the source redirected into this request.
            let src_entry = details.entry(src.clone()).or_default();
            src_entry.redirected_to = Some(request_id.clone());
        }

        Some(request)
    }

    /// Handle `Network.loadingFinished`: record encoded size + finished flag,
    /// decrement inflight.
    pub(crate) fn on_loading_finished(&self, params: &Value) {
        let request_id = match params.get("requestId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return,
        };
        let encoded = params
            .get("encodedDataLength")
            .and_then(|v| v.as_u64())
            .or_else(|| params.get("encodedDataLength").and_then(|v| v.as_f64()).map(|n| n as u64));
        let mut details = self.details.lock();
        let entry = details.entry(request_id).or_default();
        entry.finished = true;
        if let Some(enc) = encoded {
            // Merge into existing sizes if any.
            entry.sizes = Some(Sizes {
                encoded_data_length: Some(enc),
            });
        }
        drop(details);
        // inflight bookkeeping shared with the failed path.
        self.decrement_inflight();
    }

    /// Handle `Network.loadingFailed`: record failure text + decrement inflight.
    pub(crate) fn on_loading_failed(&self, params: &Value) {
        let request_id = match params.get("requestId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return,
        };
        let error_text = params
            .get("errorText")
            .and_then(|v| v.as_str())
            .map(String::from);
        let mut details = self.details.lock();
        let entry = details.entry(request_id).or_default();
        if let Some(t) = error_text {
            entry.failure = Some(t);
        }
        drop(details);
        self.decrement_inflight();
    }

    /// Shared inflight decrement with an underflow guard.
    fn decrement_inflight(&self) {
        let prev = self.inflight.fetch_sub(1, Ordering::Relaxed);
        if prev == 0 {
            // Correct an accidental underflow (event without matching request).
            self.inflight.store(0, Ordering::Relaxed);
        }
    }

    /// Handle `Network.responseReceived`. Stores + returns the response.
    pub(crate) fn on_response_received(
        &self,
        params: &Value,
        session: Arc<CdpSession>,
    ) -> Option<Response> {
        let resp = params.get("response")?;
        let request_id = params.get("requestId").and_then(|v| v.as_str())?.to_string();
        let request = self.get_request(&request_id)?;
        let url = resp.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let status = resp.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        let status_text = resp
            .get("statusText")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let headers = headers_from_value(resp.get("headers"));
        let from_disk = resp.get("fromDiskCache").and_then(|v| v.as_bool()).unwrap_or(false);
        let from_prefetch = resp
            .get("fromPrefetchCache")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let from_service_worker = resp
            .get("fromServiceWorker")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let response = Response::new(
            url,
            status,
            status_text,
            headers,
            request_id.clone(),
            from_disk || from_prefetch,
            from_service_worker,
            session,
            request,
        );
        self.responses.lock().insert(request_id.clone(), response.clone());

        // Capture extra response detail.
        let timing = Timing::from_value(resp.get("timing"));
        let security_details = SecurityDetails::from_value(resp.get("securityDetails"));
        let server_addr = ServerAddr::from_value(Some(resp));
        let http_version = resp
            .get("protocol")
            .and_then(|v| v.as_str())
            .map(String::from);
        let raw_response_headers = headers_raw_from_value(resp.get("headers"));

        let mut details = self.details.lock();
        let entry = details.entry(request_id).or_default();
        if let Some(t) = timing {
            entry.timing = Some(t);
        }
        if let Some(s) = security_details {
            entry.security_details = Some(s);
        }
        if let Some(a) = server_addr {
            entry.server_addr = Some(a);
        }
        if let Some(v) = http_version {
            entry.http_version = Some(v);
        }
        if let Some(h) = raw_response_headers {
            entry.raw_response_headers = Some(h);
        }

        Some(response)
    }
}

/// Background task: subscribe to session events, update the store, fire handlers.
pub(crate) async fn network_tracker(
    mut rx: broadcast::Receiver<CdpEvent>,
    session: Arc<CdpSession>,
    store: Arc<NetworkStore>,
    request_handlers: Arc<Mutex<Vec<RequestHandler>>>,
    response_handlers: Arc<Mutex<Vec<ResponseHandler>>>,
    failed_handlers: Arc<Mutex<Vec<RequestHandler>>>,
) {
    loop {
        match rx.recv().await {
            Ok(ev) => match ev.method.as_str() {
                "Network.requestWillBeSent" => {
                    if let Some(req) = store.on_request_will_be_sent(&ev.params) {
                        fire(&request_handlers, req);
                    }
                }
                "Network.responseReceived" => {
                    if let Some(resp) =
                        store.on_response_received(&ev.params, Arc::clone(&session))
                    {
                        fire(&response_handlers, resp);
                    }
                }
                "Network.loadingFinished" => {
                    store.on_loading_finished(&ev.params);
                }
                "Network.loadingFailed" => {
                    store.on_loading_failed(&ev.params);
                    if let Some(rid) = ev.params.get("requestId").and_then(|v| v.as_str()) {
                        if let Some(req) = store.get_request(rid) {
                            fire(&failed_handlers, req);
                        }
                    }
                }
                _ => {}
            },
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
        }
    }
}

fn fire<T: Clone + Send + 'static>(
    handlers: &Arc<Mutex<Vec<Arc<dyn Fn(T) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>>>>,
    value: T,
) {
    let list = handlers.lock().clone();
    for h in list {
        let v = value.clone();
        tokio::spawn(async move {
            (h)(v).await;
        });
    }
}

/// Lowercased header map (used for the existing `headers()` accessors).
fn headers_from_value(v: Option<&Value>) -> Headers {
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

/// Original-case header map (for `all_headers()`).
fn headers_raw_from_value(v: Option<&Value>) -> Option<Headers> {
    let obj = v.and_then(|v| v.as_object())?;
    let mut out = Headers::new();
    for (k, val) in obj {
        if let Some(s) = val.as_str() {
            out.insert(k.clone(), s.to_string());
        }
    }
    Some(out)
}
