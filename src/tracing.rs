//! `Tracing` — performance tracing via the browser-level CDP `Tracing` domain.
//!
//! `BrowserContext::tracing()` returns a [`Tracing`] handle bound to the
//! browser-level CDP session. [`Tracing::start`] begins a capture;
//! [`Tracing::stop`] ends it and returns the full trace as JSON bytes (also
//! written to `path` if one was recorded on start).
//!
//! This mirrors Playwright's `context.tracing()` shape, but speaks CDP
//! directly: `Tracing.start` with `transferMode: "ReturnAsStream"`, then on
//! stop `Tracing.end` + the `Tracing.tracingComplete` event carries a stream
//! handle, which we drain with `IO.read` until `eof`.

use crate::cdp::session::CdpSession;
use crate::cdp::CdpEvent;
use crate::error::{Error, Result};
use crate::options::TracingStartOptions;
use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::broadcast;

const SCREENSHOT_CATEGORY: &str = "disabled-by-default-devtools.screenshot";

/// A performance tracing handle for a [`BrowserContext`](crate::BrowserContext).
///
/// Cheaply cloneable; all clones share the same underlying state (the browser
/// session, an optional output path, and a started flag).
#[derive(Clone)]
pub struct Tracing {
    inner: Arc<TracingInner>,
}

struct TracingInner {
    /// The browser-level CDP session (Tracing is a browser-level domain).
    session: CdpSession,
    /// Optional file path captured from `TracingStartOptions`, written on stop.
    path: Mutex<Option<String>>,
    /// Guards against double-start / stop-without-start.
    started: Mutex<bool>,
}

impl Tracing {
    pub(crate) fn new(session: CdpSession) -> Self {
        Self {
            inner: Arc::new(TracingInner {
                session,
                path: Mutex::new(None),
                started: Mutex::new(false),
            }),
        }
    }

    /// Begin capturing a trace.
    ///
    /// Subscribes to the browser session's event stream *before* issuing
    /// `Tracing.start` so the later `Tracing.tracingComplete` event is not
    /// missed.
    pub async fn start(&self, options: Option<TracingStartOptions>) -> Result<()> {
        let opts = options.unwrap_or_default();

        // Record the output path for stop().
        *self.inner.path.lock() = opts.path.clone();

        // Build the category list. CDP `Tracing.start` takes `categories` as a
        // comma-separated string.
        let mut categories: Vec<String> = opts.categories.unwrap_or_default();
        if opts.screenshots.unwrap_or(false) && !categories.iter().any(|c| c == SCREENSHOT_CATEGORY) {
            categories.push(SCREENSHOT_CATEGORY.to_string());
        }

        let mut params = json!({
            "transferMode": "ReturnAsStream",
            "streamFormat": "json",
            "streamCompression": "none",
        });
        if !categories.is_empty() {
            params["categories"] = json!(categories.join(","));
        }

        self.inner
            .session
            .send("Tracing.start", params)
            .await
            .map(|_: Value| ())?;
        *self.inner.started.lock() = true;
        Ok(())
    }

    /// Stop the in-progress trace and return the full trace as JSON bytes.
    ///
    /// Sends `Tracing.end`, awaits `Tracing.tracingComplete` (which carries the
    /// stream handle), then drains the stream with `IO.read` until `eof`. If a
    /// `path` was recorded on start, the bytes are also written there.
    pub async fn stop(&self) -> Result<Vec<u8>> {
        {
            let mut started = self.inner.started.lock();
            if !*started {
                return Err(Error::InvalidArgument(
                    "Tracing::stop called without a preceding Tracing::start".into(),
                ));
            }
            *started = false;
        }

        // Subscribe to the browser session BEFORE sending Tracing.end so the
        // tracingComplete event is captured by this receiver.
        let mut rx = self.inner.session.subscribe();

        self.inner
            .session
            .send("Tracing.end", json!({}))
            .await
            .map(|_: Value| ())?;

        // Await Tracing.tracingComplete, which carries the stream handle.
        let stream = await_tracing_complete(&mut rx).await?;

        let stream_handle = stream
            .get("stream")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::ProtocolError(
                    "Tracing.tracingComplete did not include a stream handle".into(),
                )
            })?
            .to_string();

        // Drain the stream via IO.read until eof.
        let bytes = read_stream(&self.inner.session, &stream_handle).await?;

        // Optionally persist to the recorded path.
        if let Some(path) = self.inner.path.lock().clone() {
            if !path.is_empty() {
                tokio::fs::write(&path, &bytes).await?;
            }
        }

        Ok(bytes)
    }
}

/// Consume events from `rx` until the next `Tracing.tracingComplete`, returning
/// its params. Times out after the session default to avoid hanging forever.
async fn await_tracing_complete(rx: &mut broadcast::Receiver<CdpEvent>) -> Result<Value> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let recv = tokio::time::timeout_at(deadline, rx.recv()).await;
        match recv {
            Ok(Ok(ev)) if ev.method == "Tracing.tracingComplete" => return Ok(ev.params),
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return Err(Error::ChannelClosed);
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Err(_) => {
                return Err(Error::Timeout(
                    "timed out waiting for Tracing.tracingComplete".into(),
                ));
            }
        }
    }
}

/// Read a CDP stream handle to completion via `IO.read`, concatenating
/// base64-decoded `data` chunks until `eof: true`.
async fn read_stream(session: &CdpSession, handle: &str) -> Result<Vec<u8>> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut out = Vec::new();
    loop {
        let resp = session
            .send(
                "IO.read",
                json!({ "handle": handle }),
            )
            .await?;

        let eof = resp.get("eof").and_then(|v| v.as_bool()).unwrap_or(false);
        if let Some(data) = resp.get("base64Encoded").and_then(|v| v.as_bool()).unwrap_or(false)
            .then(|| resp.get("data"))
            .flatten()
            .and_then(|v| v.as_str())
        {
            let decoded = engine
                .decode(data)
                .map_err(|e| Error::ProtocolError(format!("base64 decode failed: {e}")))?;
            out.extend_from_slice(&decoded);
        } else if let Some(data) = resp.get("data").and_then(|v| v.as_str()) {
            // Non-base64 (text) data path — concatenate raw bytes.
            out.extend_from_slice(data.as_bytes());
        }

        if eof {
            break;
        }
    }
    Ok(out)
}
