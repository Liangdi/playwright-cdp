//! A CDP session — a session-scoped command sender plus event stream.
//!
//! `session_id == None` is the browser-level session; `Some(sid)` targets a
//! specific page/worker. This is the real CDP equivalent of Playwright's
//! `CDPSession`, speaking the protocol directly rather than proxying through
//! a driver.

use crate::cdp::connection::CdpConnection;
use crate::cdp::messages::CdpEvent;
use crate::error::{Error, Result};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// Default per-command timeout (30s), matches Playwright.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// A session over a [`CdpConnection`].
pub struct CdpSession {
    conn: Arc<CdpConnection>,
    session_id: Option<String>,
    timeout_ms: AtomicU64,
}

impl CdpSession {
    /// Browser-level session (no `sessionId`).
    pub fn browser(conn: Arc<CdpConnection>) -> Self {
        Self {
            conn,
            session_id: None,
            timeout_ms: AtomicU64::new(DEFAULT_TIMEOUT_MS),
        }
    }

    /// Target-level session for a known `sessionId`.
    pub fn target(conn: Arc<CdpConnection>, session_id: impl Into<String>) -> Self {
        Self {
            conn,
            session_id: Some(session_id.into()),
            timeout_ms: AtomicU64::new(DEFAULT_TIMEOUT_MS),
        }
    }

    /// The session id, or `None` for the browser-level session.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn set_default_timeout_ms(&self, ms: u64) {
        self.timeout_ms.store(ms, Ordering::Relaxed);
    }

    /// Send a CDP command, awaiting its response with the session's default timeout.
    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        self.send_with_timeout(method, params, self.current_timeout()).await
    }

    /// Send a CDP command with an explicit timeout.
    pub async fn send_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let fut = self.conn.send_command(self.session_id.as_deref(), method, params);
        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| {
                Error::Timeout(format!("{method} timed out after {}ms", timeout.as_millis()))
            })?
    }

    /// Send a CDP command with no timeout (e.g. for long navigations).
    pub async fn send_no_timeout(&self, method: &str, params: Value) -> Result<Value> {
        self.conn.send_command(self.session_id.as_deref(), method, params).await
    }

    fn current_timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.load(Ordering::Relaxed))
    }

    /// Subscribe to all events on this session. Filter by `event.method` at the
    /// call site. Subscribe *before* issuing the command whose events you want.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.conn.subscribe(self.session_id.clone())
    }

    /// Back-reference to the underlying connection (for cross-session coordination).
    pub fn connection(&self) -> &Arc<CdpConnection> {
        &self.conn
    }
}
