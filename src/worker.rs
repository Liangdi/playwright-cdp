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
