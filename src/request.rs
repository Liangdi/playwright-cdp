//! `Request` — an HTTP request observed via the CDP `Network` domain.

use crate::error::{Error, Result};
use crate::network::{NetworkStore, Sizes, Timing};
use crate::response::Response;
use crate::types::Headers;
use serde::de::DeserializeOwned;
use std::sync::{Arc, Weak};

/// An HTTP request captured for a page.
#[derive(Clone)]
pub struct Request {
    inner: Arc<RequestInner>,
}

struct RequestInner {
    url: String,
    method: String,
    headers: Headers,
    resource_type: String,
    request_id: String,
    is_navigation: bool,
    store: Weak<NetworkStore>,
}

impl Request {
    pub(crate) fn new(
        url: String,
        method: String,
        headers: Headers,
        resource_type: String,
        request_id: String,
        is_navigation: bool,
        store: Weak<NetworkStore>,
    ) -> Self {
        Self {
            inner: Arc::new(RequestInner {
                url,
                method,
                headers,
                resource_type,
                request_id,
                is_navigation,
                store,
            }),
        }
    }

    pub fn url(&self) -> &str {
        &self.inner.url
    }

    pub fn method(&self) -> &str {
        &self.inner.method
    }

    pub fn headers(&self) -> &Headers {
        &self.inner.headers
    }

    pub fn resource_type(&self) -> &str {
        &self.inner.resource_type
    }

    pub fn is_navigation_request(&self) -> bool {
        self.inner.is_navigation
    }

    /// The CDP `requestId` backing this request.
    pub fn request_id(&self) -> &str {
        &self.inner.request_id
    }

    fn store(&self) -> Option<Arc<NetworkStore>> {
        self.inner.store.upgrade()
    }

    /// Crate-internal access to the owning network store (for `Response`).
    pub(crate) fn store_weak(&self) -> Option<Arc<NetworkStore>> {
        self.inner.store.upgrade()
    }

    /// The response to this request, if it has arrived yet.
    pub async fn response(&self) -> Result<Option<Response>> {
        Ok(self
            .store()
            .and_then(|s| s.get_response(&self.inner.request_id)))
    }

    /// Raw request body (`Network.requestWillBeSent.postData`), if any.
    pub fn post_data(&self) -> Option<String> {
        self.store()
            .and_then(|s| s.post_data(&self.inner.request_id))
    }

    /// Raw request body as bytes.
    pub fn post_data_buffer(&self) -> Option<Vec<u8>> {
        self.post_data().map(|s| s.into_bytes())
    }

    /// Parsed request body, deserialized as `T`. Returns `None` if there is no
    /// post data; returns an error if the body is not valid JSON for `T`.
    pub fn post_data_json<T: DeserializeOwned>(&self) -> Option<Result<T>> {
        let data = self.post_data()?;
        Some(serde_json::from_str(&data).map_err(Error::from))
    }

    /// The request this one redirected from (the previous hop in the redirect
    /// chain), if any.
    pub fn redirected_from(&self) -> Option<Request> {
        let store = self.store()?;
        let prev_id = store.redirect_source(&self.inner.request_id)?;
        store.get_request(&prev_id)
    }

    /// The request this one redirected to (the next hop in the redirect chain),
    /// if any.
    pub fn redirected_to(&self) -> Option<Request> {
        let store = self.store()?;
        let next_id = store.redirect_target(&self.inner.request_id)?;
        store.get_request(&next_id)
    }

    /// Failure text captured from `Network.loadingFailed.errorText`, if the
    /// request failed to load.
    pub fn failure(&self) -> Option<String> {
        self.store()
            .and_then(|s| s.failure(&self.inner.request_id))
    }

    /// Timing information for this request, if it has been captured.
    pub fn timing(&self) -> Option<Timing> {
        self.store()
            .and_then(|s| s.timing(&self.inner.request_id))
    }

    /// Transferred size information, if `Network.loadingFinished` was received.
    pub fn sizes(&self) -> Option<Sizes> {
        self.store()
            .and_then(|s| s.sizes(&self.inner.request_id))
    }

    /// All raw request headers (original case). Falls back to the lowercased
    /// `headers()` map when raw headers were not captured.
    pub fn all_headers(&self) -> Headers {
        self.store()
            .and_then(|s| s.raw_request_headers(&self.inner.request_id))
            .unwrap_or_else(|| self.inner.headers.clone())
    }

    /// The frame that issued this request.
    ///
    /// Frame correlation is not tracked by the network store, so this always
    /// returns `None`.
    pub fn frame(&self) -> Option<crate::frame::Frame> {
        None
    }
}
