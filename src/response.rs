//! `Response` — an HTTP response observed via the CDP `Network` domain.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
use crate::network::{NetworkStore, SecurityDetails, ServerAddr, Sizes, Timing};
use crate::request::Request;
use crate::types::Headers;
use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;

/// An HTTP response captured for a page.
#[derive(Clone)]
pub struct Response {
    inner: Arc<ResponseInner>,
}

struct ResponseInner {
    url: String,
    status: u16,
    status_text: String,
    headers: Headers,
    request_id: String,
    from_cache: bool,
    from_service_worker: bool,
    session: Arc<CdpSession>,
    body: Mutex<Option<Arc<[u8]>>>,
    request: Request,
}

impl Response {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        url: String,
        status: u16,
        status_text: String,
        headers: Headers,
        request_id: String,
        from_cache: bool,
        from_service_worker: bool,
        session: Arc<CdpSession>,
        request: Request,
    ) -> Self {
        Self {
            inner: Arc::new(ResponseInner {
                url,
                status,
                status_text,
                headers,
                request_id,
                from_cache,
                from_service_worker,
                session,
                body: Mutex::new(None),
                request,
            }),
        }
    }

    pub fn url(&self) -> &str {
        &self.inner.url
    }

    pub fn status(&self) -> u16 {
        self.inner.status
    }

    pub fn status_text(&self) -> &str {
        &self.inner.status_text
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.inner.status)
    }

    pub fn headers(&self) -> &Headers {
        &self.inner.headers
    }

    pub fn from_cache(&self) -> bool {
        self.inner.from_cache
    }

    pub fn from_service_worker(&self) -> bool {
        self.inner.from_service_worker
    }

    pub fn request(&self) -> Request {
        self.inner.request.clone()
    }

    /// HTTP / protocol version reported by the server (e.g. "http/1.1",
    /// "h2"), derived from `Network.Response.protocol`. Empty string when not
    /// captured.
    pub fn http_version(&self) -> String {
        self.store()
            .and_then(|s| s.http_version(&self.inner.request_id))
            .unwrap_or_default()
    }

    /// TLS/SSL security details for the connection, if captured.
    pub fn security_details(&self) -> Option<SecurityDetails> {
        self.store()
            .and_then(|s| s.security_details(&self.inner.request_id))
    }

    /// The remote server address this response came from, if captured.
    pub fn server_addr(&self) -> Option<ServerAddr> {
        self.store()
            .and_then(|s| s.server_addr(&self.inner.request_id))
    }

    /// Transferred size information, if `Network.loadingFinished` was received.
    pub fn sizes(&self) -> Option<Sizes> {
        self.store()
            .and_then(|s| s.sizes(&self.inner.request_id))
    }

    /// Timing information for this response, if captured.
    pub fn timing(&self) -> Option<Timing> {
        self.store()
            .and_then(|s| s.timing(&self.inner.request_id))
    }

    /// All raw response headers (original case). Falls back to the lowercased
    /// `headers()` map when raw headers were not captured.
    pub fn all_headers(&self) -> Headers {
        self.store()
            .and_then(|s| s.raw_response_headers(&self.inner.request_id))
            .unwrap_or_else(|| self.inner.headers.clone())
    }

    /// Resolves successfully once `Network.loadingFinished` was observed for
    /// this response; otherwise returns an error describing why.
    pub fn finished(&self) -> Result<()> {
        if let Some(store) = self.store() {
            if store.finished(&self.inner.request_id) {
                return Ok(());
            }
            // Distinguish a known failure from a still-pending load.
            if let Some(text) = store.failure(&self.inner.request_id) {
                return Err(Error::ProtocolError(format!(
                    "request failed: {text}"
                )));
            }
        }
        Err(Error::ProtocolError(
            "request has not finished loading".to_string(),
        ))
    }

    fn store(&self) -> Option<Arc<NetworkStore>> {
        self.inner.request.store_weak()
    }

    /// Fetch the response body (cached after the first call).
    pub async fn body(&self) -> Result<Vec<u8>> {
        if let Some(b) = self.inner.body.lock().clone() {
            return Ok(b.to_vec());
        }
        let v = self
            .inner
            .session
            .send(
                "Network.getResponseBody",
                json!({ "requestId": self.inner.request_id }),
            )
            .await?;
        let body_str = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
        let encoded = v.get("base64Encoded").and_then(|x| x.as_bool()).unwrap_or(false);
        let bytes = if encoded {
            base64::engine::general_purpose::STANDARD
                .decode(body_str)
                .map_err(|e| Error::ProtocolError(format!("response body base64 decode: {e}")))?
        } else {
            body_str.as_bytes().to_vec()
        };
        *self.inner.body.lock() = Some(Arc::from(bytes.as_slice()));
        Ok(bytes)
    }

    pub async fn text(&self) -> Result<String> {
        let bytes = self.body().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn json(&self) -> Result<Value> {
        let text = self.text().await?;
        serde_json::from_str(&text).map_err(Error::from)
    }
}
