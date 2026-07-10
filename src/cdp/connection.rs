//! The multiplexing CDP connection.
//!
//! A single browser-level WebSocket carries traffic for the browser and every
//! attached target (page), each tagged with `sessionId`. This struct owns the
//! request/response correlation (by `id`) and the per-session event fan-out.

use crate::cdp::messages::{CdpEvent, CdpRequest, Message};
use crate::cdp::transport::{self, WebSocketWriter};
use crate::{Error, Result};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Broadcast capacity for per-session event channels.
const EVENT_CHANNEL_CAPACITY: usize = 128;

/// Key into the event-channel map: `None` = browser-level, `Some(sid)` = target.
type SessionKey = Option<String>;

/// The single shared CDP connection over one WebSocket.
///
/// Cheap to share via `Arc<CdpConnection>`. A background dispatch task runs
/// until the socket closes; when it does, all pending callbacks resolve to
/// [`Error::ChannelClosed`].
pub struct CdpConnection {
    next_id: AtomicU32,
    callbacks: Mutex<HashMap<u32, oneshot::Sender<Result<Value>>>>,
    event_channels: Mutex<HashMap<SessionKey, broadcast::Sender<CdpEvent>>>,
    writer: tokio::sync::Mutex<WebSocketWriter>,
}

impl CdpConnection {
    /// Connect to a CDP WebSocket endpoint and start the dispatch task.
    ///
    /// Returns the shared connection plus a receiver for browser-level events
    /// (`session_id == None`).
    pub async fn connect(
        ws_url: &str,
        headers: Option<HashMap<String, String>>,
    ) -> Result<(Arc<Self>, broadcast::Receiver<CdpEvent>)> {
        let (writer, message_rx) = transport::connect(ws_url, headers).await?;

        let conn = Arc::new(Self {
            next_id: AtomicU32::new(1),
            callbacks: Mutex::new(HashMap::new()),
            event_channels: Mutex::new(HashMap::new()),
            writer: tokio::sync::Mutex::new(writer),
        });

        let browser_rx = conn.subscribe(None);

        let conn_for_loop = Arc::clone(&conn);
        tokio::spawn(async move {
            conn_for_loop.dispatch_loop(message_rx).await;
        });

        Ok((conn, browser_rx))
    }

    /// Send a CDP command and await its response (no timeout — wrap at a
    /// higher layer if needed). `session_id` is `None` for browser-level.
    pub async fn send_command(
        self: &Arc<Self>,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel::<Result<Value>>();
        self.callbacks.lock().insert(id, tx);

        let request = CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.map(|s| s.to_string()),
        };
        let value = serde_json::to_value(&request)?;

        // Serialize whole-message writes so frames never interleave.
        let send_result = self.writer.lock().await.send(&value).await;
        if let Err(e) = send_result {
            self.callbacks.lock().remove(&id);
            return Err(e);
        }

        match rx.await {
            Ok(res) => res,
            Err(_) => {
                self.callbacks.lock().remove(&id);
                Err(Error::ChannelClosed)
            }
        }
    }

    /// Subscribe to events for a session. `None` = browser-level. New
    /// subscribers only receive events emitted *after* this call — subscribe
    /// before issuing the command whose events you want to observe.
    pub fn subscribe(&self, session_id: SessionKey) -> broadcast::Receiver<CdpEvent> {
        self.event_channels
            .lock()
            .entry(session_id)
            .or_insert_with(|| broadcast::channel(EVENT_CHANNEL_CAPACITY).0)
            .subscribe()
    }

    /// Best-effort graceful close of the underlying socket.
    pub async fn close(&self) -> Result<()> {
        self.writer.lock().await.close().await
    }

    async fn dispatch_loop(self: Arc<Self>, mut message_rx: mpsc::Receiver<Value>) {
        while let Some(v) = message_rx.recv().await {
            match serde_json::from_value::<Message>(v) {
                Ok(Message::Response(r)) => {
                    let result = match r.error {
                        Some(e) => Err(Error::CdpError {
                            code: e.code,
                            message: e.message,
                            data: e.data,
                        }),
                        None => Ok(r.result.unwrap_or(Value::Null)),
                    };
                    if let Some(cb) = self.callbacks.lock().remove(&r.id) {
                        let _ = cb.send(result);
                    } else {
                        tracing::trace!(id = r.id, "orphan CDP response");
                    }
                }
                Ok(Message::Event(e)) => {
                    let key: SessionKey = e.session_id.clone();
                    let channel = self
                        .event_channels
                        .lock()
                        .entry(key)
                        .or_insert_with(|| broadcast::channel(EVENT_CHANNEL_CAPACITY).0)
                        .clone();
                    // broadcast::send returns Err if there are no receivers;
                    // that's fine — events with no subscribers are dropped.
                    let _ = channel.send(e);
                }
                Err(err) => {
                    tracing::error!(error = %err, "failed to parse CDP message");
                }
            }
        }

        tracing::debug!("CDP transport closed; failing pending callbacks");
        // Dropping the callbacks resolves their `rx` with a RecvError → ChannelClosed.
        self.callbacks.lock().clear();
    }
}

// Unit tests for the dispatch logic live behind an integration test that uses
// a real (or loopback) socket — `CdpConnection` cannot be constructed in a pure
// unit test because `WebSocketWriter` requires a live WebSocket.
