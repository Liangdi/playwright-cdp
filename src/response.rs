//! `Response` — an HTTP response observed via the CDP `Network` domain.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
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
