//! Network capture: stores HTTP requests/responses observed via the CDP
//! `Network` domain and drives `Page::on_request` / `on_response` handlers.

use crate::cdp::session::CdpSession;
use crate::cdp::CdpEvent;
use crate::request::Request;
use crate::response::Response;
use crate::types::Headers;
use parking_lot::Mutex;
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

/// Per-page store of observed requests/responses, keyed by CDP `requestId`.
pub(crate) struct NetworkStore {
    self_ref: Mutex<Weak<NetworkStore>>,
    requests: Mutex<HashMap<String, Request>>,
    responses: Mutex<HashMap<String, Response>>,
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
        self.requests.lock().insert(request_id, request.clone());
        self.inflight.fetch_add(1, Ordering::Relaxed);
        Some(request)
    }

    /// Handle `Network.loadingFinished` / `Network.loadingFailed`: decrement inflight.
    pub(crate) fn on_loading_settled(&self, params: &Value) {
        if params.get("requestId").and_then(|v| v.as_str()).is_some() {
            // fetch_sub clamped at 0 (underflow guard via min).
            let prev = self.inflight.fetch_sub(1, Ordering::Relaxed);
            if prev == 0 {
                // Correct an accidental underflow (event without matching request).
                self.inflight.store(0, Ordering::Relaxed);
            }
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
        self.responses.lock().insert(request_id, response.clone());
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
                    if let Some(resp) = store.on_response_received(&ev.params, Arc::clone(&session))
                    {
                        fire(&response_handlers, resp);
                    }
                }
                "Network.loadingFinished" => {
                    store.on_loading_settled(&ev.params);
                }
                "Network.loadingFailed" => {
                    store.on_loading_settled(&ev.params);
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
