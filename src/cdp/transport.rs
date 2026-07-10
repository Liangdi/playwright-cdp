//! WebSocket transport for CDP.
//!
//! One JSON object per WebSocket text frame — exactly CDP's wire format. This
//! is a trimmed, self-contained version of `playwright-rust`'s transport (no
//! driver dependency, no trait gymnastics): `connect` returns a writer half
//! plus a bounded receiver of parsed JSON values, and spawns the read pump.

use crate::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::str::FromStr;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Capacity of the transport-to-dispatch channel. Bounded so a slow dispatch
/// loop exerts backpressure on the socket instead of buffering unboundedly.
pub(crate) const MESSAGE_CHANNEL_CAPACITY: usize = 256;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connect to a CDP WebSocket endpoint.
///
/// Returns a [`WebSocketWriter`] (the sending half) and a bounded receiver of
/// incoming parsed JSON messages. A background task pumps the read side.
pub async fn connect(
    url: &str,
    headers: Option<HashMap<String, String>>,
) -> Result<(WebSocketWriter, mpsc::Receiver<Value>)> {
    let mut request = url
        .into_client_request()
        .map_err(|e| Error::TransportError(format!("invalid CDP endpoint {url}: {e}")))?;

    if let Some(extra) = headers {
        let h = request.headers_mut();
        for (k, v) in extra {
            let name = HeaderName::from_str(&k)
                .map_err(|e| Error::TransportError(format!("bad header name {k}: {e}")))?;
            let value = HeaderValue::from_str(&v)
                .map_err(|e| Error::TransportError(format!("bad header value: {e}")))?;
            h.insert(name, value);
        }
    }

    let (ws_stream, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| Error::ConnectionFailed(format!("WebSocket connect to {url} failed: {e}")))?;

    let (sink, stream) = ws_stream.split();
    let (tx, rx) = mpsc::channel::<Value>(MESSAGE_CHANNEL_CAPACITY);

    tokio::spawn(pump_reads(stream, tx));

    Ok((WebSocketWriter { sink }, rx))
}

async fn pump_reads(
    mut stream: futures_util::stream::SplitStream<WsStream>,
    tx: mpsc::Sender<Value>,
) {
    while let Some(msg_result) = stream.next().await {
        match msg_result {
            Ok(WsMessage::Text(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(v) => {
                    if tx.send(v).await.is_err() {
                        break; // dispatch side gone
                    }
                }
                Err(e) => {
                    tracing::trace!(error = %e, "ignoring non-JSON CDP frame");
                }
            },
            Ok(WsMessage::Binary(_)) => {} // CDP uses Text frames only
            Ok(WsMessage::Close(_)) | Err(_) => break,
            // Ping/Pong handled by tungstenite automatically.
            _ => {}
        }
    }
}

/// Sending half of the CDP WebSocket connection.
pub struct WebSocketWriter {
    sink: futures_util::stream::SplitSink<WsStream, WsMessage>,
}

impl WebSocketWriter {
    /// Send a JSON value as a single text frame.
    pub async fn send(&mut self, value: &Value) -> Result<()> {
        let s = serde_json::to_string(value)?;
        self.sink
            .send(WsMessage::Text(s.into()))
            .await
            .map_err(|e| Error::TransportError(format!("WebSocket send failed: {e}")))?;
        Ok(())
    }

    /// Close the socket cleanly.
    pub async fn close(&mut self) -> Result<()> {
        let _ = self
            .sink
            .close()
            .await
            .map_err(|e| Error::TransportError(format!("WebSocket close failed: {e}")));
        Ok(())
    }
}
