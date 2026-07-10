//! `Request` — an HTTP request observed via the CDP `Network` domain.

use crate::error::Result;
use crate::network::NetworkStore;
use crate::response::Response;
use crate::types::Headers;
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

    /// The response to this request, if it has arrived yet.
    pub async fn response(&self) -> Result<Option<Response>> {
        Ok(self
            .inner
            .store
            .upgrade()
            .and_then(|s| s.get_response(&self.inner.request_id)))
    }
}
