//! `WebStorage` — `localStorage` / `sessionStorage` access via the CDP
//! `DOMStorage` domain.
//!
//! A [`WebStorage`] is bound to a page session, a security origin, and a choice
//! of local vs. session storage. All operations map directly onto CDP's
//! `DOMStorage.*` commands with a `storageId` of `{ securityOrigin,
//! isLocalStorage }`:
//!
//! | Method                    | CDP command                |
//! |---------------------------|----------------------------|
//! | [`WebStorage::get`]       | `DOMStorage.getDOMStorageItems` (client-side filter) |
//! | [`WebStorage::set`]       | `DOMStorage.setDOMStorageItem` |
//! | [`WebStorage::remove`]    | `DOMStorage.removeDOMStorageItem` |
//! | [`WebStorage::clear`]     | `DOMStorage.clear` |
//! | [`WebStorage::keys`]      | `DOMStorage.getDOMStorageItems` |
//! | [`WebStorage::size`]      | `DOMStorage.getDOMStorageItems` |
//! | [`WebStorage::has`]       | `DOMStorage.getDOMStorageItems` (client-side filter) |
//!
//! Mirrors the shape of Playwright's `page.evaluate`-backed storage helpers,
//! but talks CDP directly.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
use serde_json::{json, Value};
use std::sync::Arc;

/// A handle to one of a page's web storages (`localStorage` or
/// `sessionStorage`) for a given origin.
///
/// Cheaply cloneable; all clones share the same page session, origin, and
/// storage kind.
#[derive(Clone)]
pub struct WebStorage {
    inner: Arc<WebStorageInner>,
}

struct WebStorageInner {
    /// The page-level CDP session (DOMStorage lives here).
    session: CdpSession,
    /// The security origin the storage is scoped to (e.g. `https://example.com`).
    security_origin: String,
    /// `true` for `localStorage`, `false` for `sessionStorage`.
    is_local_storage: bool,
}

impl WebStorage {
    pub(crate) fn new(session: CdpSession, security_origin: String, is_local_storage: bool) -> Self {
        Self {
            inner: Arc::new(WebStorageInner {
                session,
                security_origin,
                is_local_storage,
            }),
        }
    }

    /// Build a `localStorage` handle for the given origin on `session`.
    pub(crate) fn local_storage(session: CdpSession, security_origin: String) -> Self {
        Self::new(session, security_origin, true)
    }

    /// Build a `sessionStorage` handle for the given origin on `session`.
    pub(crate) fn session_storage(session: CdpSession, security_origin: String) -> Self {
        Self::new(session, security_origin, false)
    }

    /// The CDP `storageId` object for this handle.
    fn storage_id(&self) -> Value {
        json!({
            "securityOrigin": self.inner.security_origin,
            "isLocalStorage": self.inner.is_local_storage,
        })
    }

    /// Fetch all `[key, value]` pairs from storage via
    /// `DOMStorage.getDOMStorageItems`.
    async fn items(&self) -> Result<Vec<(String, String)>> {
        let resp: Value = self
            .inner
            .session
            .send(
                "DOMStorage.getDOMStorageItems",
                json!({ "storageId": self.storage_id() }),
            )
            .await?;
        let arr = resp
            .get("entries")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::ProtocolError("getDOMStorageItems missing 'entries'".into()))?;
        let mut out = Vec::with_capacity(arr.len());
        for entry in arr {
            let mut it = entry.as_array().into_iter().flatten();
            let key = it
                .next()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let value = it
                .next()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push((key, value));
        }
        Ok(out)
    }

    /// The value for `key`, or `None` if absent.
    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let items = self.items().await?;
        Ok(items
            .into_iter()
            .find_map(|(k, v)| if k == key { Some(v) } else { None }))
    }

    /// Set `key` to `value`, overwriting any existing value. Issues
    /// `DOMStorage.setDOMStorageItem`.
    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.inner
            .session
            .send(
                "DOMStorage.setDOMStorageItem",
                json!({
                    "storageId": self.storage_id(),
                    "key": key,
                    "value": value,
                }),
            )
            .await?;
        Ok(())
    }

    /// Remove `key` (no-op if absent). Issues `DOMStorage.removeDOMStorageItem`.
    pub async fn remove(&self, key: &str) -> Result<()> {
        self.inner
            .session
            .send(
                "DOMStorage.removeDOMStorageItem",
                json!({
                    "storageId": self.storage_id(),
                    "key": key,
                }),
            )
            .await?;
        Ok(())
    }

    /// Remove all entries. Issues `DOMStorage.clear`.
    pub async fn clear(&self) -> Result<()> {
        self.inner
            .session
            .send(
                "DOMStorage.clear",
                json!({ "storageId": self.storage_id() }),
            )
            .await?;
        Ok(())
    }

    /// All keys currently in storage.
    pub async fn keys(&self) -> Result<Vec<String>> {
        Ok(self.items().await?.into_iter().map(|(k, _)| k).collect())
    }

    /// The number of entries in storage.
    pub async fn size(&self) -> Result<usize> {
        Ok(self.items().await?.len())
    }

    /// Whether `key` is present.
    pub async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.items().await?.iter().any(|(k, _)| k == key))
    }

    /// The security origin this storage is scoped to.
    pub fn security_origin(&self) -> &str {
        &self.inner.security_origin
    }

    /// `true` if this is `localStorage`, `false` for `sessionStorage`.
    pub fn is_local_storage(&self) -> bool {
        self.inner.is_local_storage
    }
}
