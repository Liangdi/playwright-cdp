//! `Worker` — web/service/shared-worker capture via the CDP `Target` domain.
//!
//! [`Page::on_worker`](crate::page::Page::on_worker) enables flattened auto-attach
//! on the page session (`Target.setAutoAttach { flatten: true }`) and dispatches a
//! [`Worker`] for every child target whose type is `worker`, `service_worker`, or
//! `shared_worker` (surfaced via `Target.attachedToTarget`). Each worker gets its
//! own sub-session on the same flattened connection; evaluation runs there via
//! `Runtime.evaluate`.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A web/service/shared worker owned by a page.
///
/// Produced by [`Page::on_worker`](crate::page::Page::on_worker). The worker is
/// driven through its own CDP sub-session; call [`evaluate`](Worker::evaluate)
/// to run JS in the worker's execution context.
#[derive(Clone)]
pub struct Worker {
    inner: Arc<WorkerInner>,
}

struct WorkerInner {
    url: String,
    session: CdpSession,
}

impl Worker {
    pub(crate) fn new(connection: Arc<crate::cdp::connection::CdpConnection>, session_id: String, url: String) -> Self {
        let session = CdpSession::target(connection, session_id);
        // Best-effort: enable the Runtime domain on the worker session so
        // `Runtime.evaluate` works. We don't await this here (we're in a sync
        // ctor); the listener task fires it after construction.
        Self {
            inner: Arc::new(WorkerInner { url, session }),
        }
    }

    /// The worker's script URL (e.g. `blob:...`, `https://.../sw.js`).
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// (Internal) the worker's CDP sub-session.
    #[allow(dead_code)]
    pub(crate) fn session(&self) -> &CdpSession {
        &self.inner.session
    }

    /// Best-effort `Runtime.enable` on the worker session so evaluate works.
    pub(crate) async fn enable_runtime(&self) {
        let _ = self.inner.session.send("Runtime.enable", json!({})).await;
    }

    /// Register a handler invoked once when the worker is destroyed.
    ///
    /// A worker is reported as destroyed when the browser detaches its target
    /// (`Target.detachedFromTarget` whose `sessionId` matches this worker's
    /// sub-session). That event is delivered on the parent session, so we
    /// subscribe at the connection (browser/root) level and filter by session
    /// id. We also watch the worker's own session for
    /// `Runtime.executionContextDestroyed` as a fallback. The handler fires at
    /// most once: the first signal wins, the other is suppressed.
    pub fn on_close<F, Fut>(&self, handler: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let session_id = match self.inner.session.session_id() {
            Some(s) => s.to_string(),
            // No session id to track — nothing to observe; drop the handler.
            None => return,
        };
        let conn = self.inner.session.connection().clone();

        // Box the handler so both watcher tasks can share a single, one-shot
        // invocation slot.
        let boxed: Arc<
            dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                + Send
                + Sync,
        > = Arc::new(move || Box::pin(handler()));
        let fired = Arc::new(AtomicBool::new(false));

        // Subscribe at the browser/root level: `Target.detachedFromTarget`
        // for the worker arrives on the parent session, not the worker's own.
        let mut root_rx = conn.subscribe(None);
        let boxed_root = Arc::clone(&boxed);
        let fired_root = Arc::clone(&fired);
        tokio::spawn(async move {
            loop {
                let ev = match root_rx.recv().await {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                if ev.method != "Target.detachedFromTarget" {
                    continue;
                }
                let detached = ev
                    .params
                    .get("sessionId")
                    .or_else(|| ev.params.get("session_id"))
                    .and_then(|v| v.as_str());
                if detached == Some(session_id.as_str()) {
                    if !fired_root.swap(true, Ordering::SeqCst) {
                        (boxed_root)().await;
                    }
                    break;
                }
            }
        });

        // Fallback: watch the worker's own session for context teardown.
        let mut self_rx = self.inner.session.subscribe();
        let boxed_ctx = Arc::clone(&boxed);
        let fired_ctx = Arc::clone(&fired);
        tokio::spawn(async move {
            loop {
                let ev = match self_rx.recv().await {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                if ev.method == "Runtime.executionContextDestroyed"
                    && !fired_ctx.swap(true, Ordering::SeqCst)
                {
                    (boxed_ctx)().await;
                    break;
                }
            }
        });
    }

    /// Evaluate a JS expression in the worker's execution context, returning a
    /// typed result.
    ///
    /// `expression` is wrapped as `(() => { return (<expression>); })()` and
    /// the result's `value` is parsed with serde.
    pub async fn evaluate<R: DeserializeOwned>(&self, expression: &str) -> Result<R> {
        let wrapped = format!("(() => {{ return ({expression}); }})()");
        let resp = self
            .inner
            .session
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": wrapped,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(exc) = resp.get("exceptionDetails") {
            let msg = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("evaluation threw");
            return Err(Error::ProtocolError(format!("eval error: {msg}")));
        }
        let value = resp
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null);
        serde_json::from_value::<R>(value).map_err(Error::from)
    }
}
