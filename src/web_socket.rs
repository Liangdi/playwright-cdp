//! `WebSocket` — a captured WebSocket connection, reconstructed from CDP
//! `Network.webSocket*` events.
//!
//! CDP does not expose a live WebSocket object; instead the `Network` domain
//! emits a lifecycle of events for each connection, keyed by `requestId`:
//!
//! - `Network.webSocketCreated` — the URL (and optional request).
//! - `Network.webSocketWillSendHandshakeRequest` — outgoing request headers.
//! - `Network.webSocketHandshakeResponseReceived` — status code + response headers.
//! - `Network.webSocketFrameSent` — a frame the page sent.
//! - `Network.webSocketFrameReceived` — a frame the page received.
//! - `Network.webSocketFrameError` — a protocol error.
//! - `Network.webSocketClosed` — the connection closed (with a timestamp).
//!
//! [`WebSocketRegistry`] subscribes to the page session, partitions events by
//! `requestId`, and accumulates them into one [`WebSocket`] per connection. The
//! page-side `page.on('websocket')` wiring (and the `WebSocket::new` the page
//! would hand callers) arrives in a later wave — this module provides the type
//! and the capture/registry logic only.
//!
//! The accumulated `WebSocket` is cheaply cloneable (`Arc<Inner>`); clones share
//! the same live state, so frames arriving after the handle is handed out are
//! visible via [`frames`](WebSocket::frames) and the `on_*` handlers.

use crate::cdp::session::CdpSession;
use crate::cdp::CdpEvent;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex as AsyncMutex};

/// A single WebSocket frame payload, tagged with its direction.
#[derive(Debug, Clone)]
pub struct WebSocketFrame {
    /// Direction relative to the page.
    pub direction: FrameDirection,
    /// The raw payload data. CDP distinguishes text and binary; for text frames
    /// this is the decoded string, for binary frames it is the raw bytes.
    pub data: FrameData,
    /// The opcode (1 = text, 2 = binary, 8 = close, 9 = ping, 10 = pong), when
    /// reported by CDP.
    pub opcode: Option<i64>,
}

/// Direction of a [`WebSocketFrame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDirection {
    /// Page → server.
    Sent,
    /// Server → page.
    Received,
}

/// A frame's payload, distinguishing text from binary.
#[derive(Debug, Clone)]
pub enum FrameData {
    /// A text frame (decoded UTF-8).
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
}

/// The WebSocket handshake result.
#[derive(Debug, Clone, Default)]
pub struct WebSocketHandshake {
    /// HTTP status code of the handshake response (e.g. 101).
    pub status: Option<i64>,
    /// Status text of the handshake response.
    pub status_text: Option<String>,
    /// Response headers, as reported by `webSocketHandshakeResponse`.
    pub response_headers: HashMap<String, String>,
    /// Request headers, as reported by `webSocketWillSendHandshakeRequest`.
    pub request_headers: HashMap<String, String>,
}

/// Information about a closed WebSocket connection.
#[derive(Debug, Clone, Default)]
pub struct WebSocketCloseInfo {
    /// Wall-time timestamp (seconds since epoch) reported by
    /// `webSocketClosed`, if any.
    pub timestamp: Option<f64>,
}

/// A captured WebSocket connection.
///
/// State is shared across clones (via `Arc<Inner>`); frames and close info
/// accumulate as CDP events arrive on the registry's subscription.
#[derive(Clone)]
pub struct WebSocket {
    inner: Arc<WebSocketInner>,
}

struct WebSocketInner {
    /// The CDP `requestId` that identifies this connection.
    request_id: String,
    /// The WebSocket URL.
    url: Mutex<String>,
    /// Accumulated handshake.
    handshake: Mutex<WebSocketHandshake>,
    /// Accumulated frames (sent + received), in arrival order.
    frames: Mutex<Vec<WebSocketFrame>>,
    /// Protocol error payload, if `webSocketFrameError` fired.
    error: Mutex<Option<String>>,
    /// Close info, once `webSocketClosed` fires.
    close: Mutex<Option<WebSocketCloseInfo>>,
    /// Broadcast bus for live frame/close/error events (see `on_*` handlers).
    events: broadcast::Sender<WebSocketLiveEvent>,
}

/// A live event emitted on a [`WebSocket`]'s broadcast bus.
#[derive(Debug, Clone)]
pub enum WebSocketLiveEvent {
    /// A frame was sent or received.
    Frame(WebSocketFrame),
    /// A protocol error occurred.
    Error(String),
    /// The connection closed.
    Closed(WebSocketCloseInfo),
}

impl WebSocket {
    /// Construct a captured connection for `request_id`, seeded with `url`.
    ///
    /// This is `pub(crate)` so the registry (and, later, the page) can mint
    /// handles; callers should obtain a `WebSocket` from the page's
    /// `on('websocket')` handler.
    pub(crate) fn new(request_id: impl Into<String>, url: impl Into<String>) -> Self {
        let (tx, _rx) = broadcast::channel(256);
        Self {
            inner: Arc::new(WebSocketInner {
                request_id: request_id.into(),
                url: Mutex::new(url.into()),
                handshake: Mutex::new(WebSocketHandshake::default()),
                frames: Mutex::new(Vec::new()),
                error: Mutex::new(None),
                close: Mutex::new(None),
                events: tx,
            }),
        }
    }

    /// The CDP `requestId` identifying this connection.
    pub fn request_id(&self) -> &str {
        &self.inner.request_id
    }

    /// The WebSocket URL.
    pub fn url(&self) -> String {
        self.inner.url.lock().clone()
    }

    /// A snapshot of the handshake (request + response headers, status).
    pub fn handshake(&self) -> WebSocketHandshake {
        self.inner.handshake.lock().clone()
    }

    /// A snapshot of all accumulated frames, in arrival order.
    pub fn frames(&self) -> Vec<WebSocketFrame> {
        self.inner.frames.lock().clone()
    }

    /// The protocol error reported by `webSocketFrameError`, if any.
    pub fn error(&self) -> Option<String> {
        self.inner.error.lock().clone()
    }

    /// Close info, once the connection has closed. `None` until
    /// `webSocketClosed` arrives.
    pub fn close_info(&self) -> Option<WebSocketCloseInfo> {
        self.inner.close.lock().clone()
    }

    // --- live event handlers (mirror the page on_* pattern) -----------------

    /// Register a handler invoked for every frame (sent or received) on this
    /// connection, mirroring Playwright's per-connection event surface.
    pub fn on_frame<F, Fut>(&self, handler: F)
    where
        F: Fn(WebSocketFrame) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.events.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if let WebSocketLiveEvent::Frame(f) = ev {
                    handler(f).await;
                }
            }
        });
    }

    /// Register a handler invoked for every frame the page receives.
    pub fn on_frame_received<F, Fut>(&self, handler: F)
    where
        F: Fn(WebSocketFrame) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.events.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if let WebSocketLiveEvent::Frame(f) = ev {
                    if f.direction == FrameDirection::Received {
                        handler(f).await;
                    }
                }
            }
        });
    }

    /// Register a handler invoked for every frame the page sends.
    pub fn on_frame_sent<F, Fut>(&self, handler: F)
    where
        F: Fn(WebSocketFrame) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.events.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if let WebSocketLiveEvent::Frame(f) = ev {
                    if f.direction == FrameDirection::Sent {
                        handler(f).await;
                    }
                }
            }
        });
    }

    /// Register a handler invoked when the connection closes.
    pub fn on_close<F, Fut>(&self, handler: F)
    where
        F: Fn(WebSocketCloseInfo) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.events.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if let WebSocketLiveEvent::Closed(c) = ev {
                    handler(c).await;
                }
            }
        });
    }

    /// Wait for the next live event of any kind, or time out after `timeout`.
    ///
    /// This consumes from a fresh subscription, so it observes only events that
    /// arrive *after* the call (it is not a replay of accumulated state — use
    /// [`frames`](Self::frames)/[`close_info`](Self::close_info) for history).
    pub async fn wait_for_event(&self, timeout: Duration) -> Result<WebSocketLiveEvent, WaitError> {
        let mut rx = self.inner.events.subscribe();
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Ok(ev)) => Ok(ev),
            Ok(Err(broadcast::error::RecvError::Closed)) => Err(WaitError::Closed),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => Err(WaitError::Lagged),
            Err(_) => Err(WaitError::Timeout),
        }
    }

    // --- ingest (used by the registry) --------------------------------------

    /// Apply a CDP `Network.webSocket*` event to this connection's state.
    /// Unknown methods are ignored.
    fn ingest(&self, method: &str, params: &Value) {
        match method {
            "Network.webSocketCreated" => {
                if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
                    *self.inner.url.lock() = url.to_string();
                }
            }
            "Network.webSocketWillSendHandshakeRequest" => {
                if let Some(req) = params.get("request") {
                    let mut hs = self.inner.handshake.lock();
                    if let Some(headers) = req.get("headers").and_then(|v| v.as_object()) {
                        hs.request_headers = headers_to_map(headers);
                    }
                }
            }
            // Chrome emits the handshake response as
            // `Network.webSocketHandshakeResponseReceived` (note the `Received`
            // suffix). Older CDP revisions used the unsuffixed
            // `Network.webSocketHandshakeResponse`; accept both for safety.
            "Network.webSocketHandshakeResponseReceived"
            | "Network.webSocketHandshakeResponse" => {
                if let Some(resp) = params.get("response") {
                    let mut hs = self.inner.handshake.lock();
                    hs.status = resp.get("status").and_then(|v| v.as_i64());
                    hs.status_text = resp
                        .get("statusText")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    if let Some(headers) = resp.get("headers").and_then(|v| v.as_object()) {
                        hs.response_headers = headers_to_map(headers);
                    }
                }
            }
            "Network.webSocketFrameSent" => {
                self.record_frame(FrameDirection::Sent, params);
            }
            "Network.webSocketFrameReceived" => {
                self.record_frame(FrameDirection::Received, params);
            }
            "Network.webSocketFrameError" => {
                let msg = params
                    .get("errorMessage")
                    .and_then(|v| v.as_str())
                    .unwrap_or("websocket frame error")
                    .to_string();
                *self.inner.error.lock() = Some(msg.clone());
                let _ = self.inner.events.send(WebSocketLiveEvent::Error(msg));
            }
            "Network.webSocketClosed" => {
                let info = WebSocketCloseInfo {
                    timestamp: params.get("timestamp").and_then(|v| v.as_f64()),
                };
                *self.inner.close.lock() = Some(info.clone());
                let _ = self.inner.events.send(WebSocketLiveEvent::Closed(info));
            }
            _ => {}
        }
    }

    /// Parse a `webSocketFrame*` payload's `response` (the frame's
    /// `opcode`/`payloadData`) and record + broadcast it.
    fn record_frame(&self, direction: FrameDirection, params: &Value) {
        let frame = params
            .get("response")
            .and_then(|f| parse_frame(direction, f));
        if let Some(frame) = frame {
            self.inner.frames.lock().push(frame.clone());
            let _ = self.inner.events.send(WebSocketLiveEvent::Frame(frame));
        }
    }
}

/// Parse one CDP `webSocketFrame` object (`{ opcode, mask, payloadData }`) into
/// a [`WebSocketFrame`]. Returns `None` if the structure is unexpected.
fn parse_frame(direction: FrameDirection, frame: &Value) -> Option<WebSocketFrame> {
    let opcode = frame.get("opcode").and_then(|v| v.as_i64());
    let payload = frame.get("payloadData").and_then(|v| v.as_str())?;
    // CDP reports both text and binary frames as a `payloadData` string. Binary
    // frames are signalled by opcode 2 (and the string may contain non-UTF-8
    // when lossy-decoded by the browser). We treat opcode 2 as binary; here we
    // only have the decoded string, so we store its bytes.
    let data = if opcode == Some(2) {
        FrameData::Binary(payload.as_bytes().to_vec())
    } else {
        FrameData::Text(payload.to_string())
    };
    Some(WebSocketFrame {
        direction,
        data,
        opcode,
    })
}

/// Flatten a CDP headers object into a `String`-keyed map. Header lookups in
/// CDP are case-insensitive in practice; we preserve the reported casing.
fn headers_to_map(headers: &serde_json::Map<String, Value>) -> HashMap<String, String> {
    let mut out = HashMap::with_capacity(headers.len());
    for (k, v) in headers {
        let val = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.insert(k.clone(), val);
    }
    out
}

/// Errors from [`WebSocket::wait_for_event`].
#[derive(Debug)]
pub enum WaitError {
    /// No event arrived before the deadline.
    Timeout,
    /// The event bus was closed (the page/session went away).
    Closed,
    /// The receiver lagged behind and dropped events.
    Lagged,
}

// ---------------------------------------------------------------------------
// Registry / factory
// ---------------------------------------------------------------------------

/// A registry that captures WebSocket connections from a page session.
///
/// Subscribes to the session's event stream (before `Network` is expected to
/// emit, though the page session already has `Network` enabled) and routes each
/// `Network.webSocket*` event to the matching [`WebSocket`], keyed by
/// `requestId`. The page can later iterate [`all`](Self::all) or look one up by
/// id when wiring its `on('websocket')` handler.
///
/// Cheaply cloneable; clones share the same capture state.
#[derive(Clone)]
pub struct WebSocketRegistry {
    inner: Arc<WebSocketRegistryInner>,
}

struct WebSocketRegistryInner {
    /// `requestId` → live `WebSocket`.
    sockets: AsyncMutex<HashMap<String, WebSocket>>,
    /// Broadcast bus announcing newly-created connections (one event per
    /// `webSocketCreated`).
    created: broadcast::Sender<WebSocket>,
}

impl WebSocketRegistry {
    pub(crate) fn new() -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            inner: Arc::new(WebSocketRegistryInner {
                sockets: AsyncMutex::new(HashMap::new()),
                created: tx,
            }),
        }
    }

    /// Begin capturing WebSocket events from `session`. Subscribes to the
    /// session event stream and spawns a dispatcher task that partitions
    /// `Network.webSocket*` events by `requestId`. Returns immediately; capture
    /// continues for the life of the spawned task.
    ///
    /// Subscribe-first is honoured: the subscription is taken *before* any
    /// further `Network` interaction so no events are missed.
    pub fn attach(&self, session: &Arc<CdpSession>) {
        let mut rx = session.subscribe();
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => dispatch(&inner, &ev).await,
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });
    }

    /// Look up a captured connection by `requestId`.
    pub async fn get(&self, request_id: &str) -> Option<WebSocket> {
        self.inner.sockets.lock().await.get(request_id).cloned()
    }

    /// All connections captured so far.
    pub async fn all(&self) -> Vec<WebSocket> {
        self.inner.sockets.lock().await.values().cloned().collect()
    }

    /// Subscribe to newly-created connections (one event per
    /// `webSocketCreated`).
    pub fn on_created(&self) -> broadcast::Receiver<WebSocket> {
        self.inner.created.subscribe()
    }
}

/// Route one CDP event to the right `WebSocket`, creating it on first sight.
async fn dispatch(reg: &WebSocketRegistryInner, ev: &CdpEvent) {
    // Every webSocket* event carries a `requestId` (except in malformed input).
    let request_id = match ev.params.get("requestId").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return,
    };

    // webSocketCreated is the lifecycle start: mint the WebSocket and announce it.
    if ev.method == "Network.webSocketCreated" {
        let url = ev
            .params
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let ws = WebSocket::new(&request_id, &url);
        let mut sockets = reg.sockets.lock().await;
        sockets.entry(request_id.clone()).or_insert_with(|| ws.clone());
        // Drop the guard before broadcasting.
        drop(sockets);
        let _ = reg.created.send(ws);
        return;
    }

    // All other events target an existing connection; if we never saw
    // webSocketCreated (e.g. subscription started mid-connection), materialize
    // an empty one on demand.
    let ws = {
        let mut sockets = reg.sockets.lock().await;
        sockets
            .entry(request_id.clone())
            .or_insert_with(|| WebSocket::new(&request_id, ""))
            .clone()
    };
    ws.ingest(&ev.method, &ev.params);
}
