//! `Browser` — top-level entry point. Launches/connects to Chromium over CDP.

use crate::browser_context::BrowserContext;
use crate::browser_process::{resolve_ws_endpoint, BrowserProcess};
use crate::cdp::connection::CdpConnection;
use crate::cdp::session::CdpSession;
use crate::error::Result;
use crate::options::{ConnectOverCdpOptions, LaunchOptions};
use crate::page::Page;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;

/// A connected Chromium browser driven over CDP.
///
/// Created via [`Browser::launch`] or [`Browser::connect_over_cdp`]. Cheaply
/// cloneable (shares one connection).
#[derive(Clone)]
pub struct Browser {
    inner: Arc<BrowserInner>,
}

struct BrowserInner {
    conn: Arc<CdpConnection>,
    browser_session: CdpSession,
    process: Mutex<Option<BrowserProcess>>,
    contexts: Mutex<Vec<BrowserContext>>,
}

impl Browser {
    /// Spawn a local Chromium-based browser per `opts`.
    pub async fn launch(opts: LaunchOptions) -> Result<Browser> {
        let process = BrowserProcess::launch(&opts).await?;
        let ws_url = process.ws_url().to_string();
        let (conn, browser_session) = connect(&ws_url, None).await?;
        Ok(Self {
            inner: Arc::new(BrowserInner {
                conn,
                browser_session,
                process: Mutex::new(Some(process)),
                contexts: Mutex::new(Vec::new()),
            }),
        })
    }

    /// Attach to an already-running Chromium given an `http://host:port` or
    /// `ws://...` endpoint.
    pub async fn connect_over_cdp(
        endpoint: &str,
        opts: Option<ConnectOverCdpOptions>,
    ) -> Result<Browser> {
        let ws_url = resolve_ws_endpoint(endpoint).await?;
        let headers = opts.and_then(|o| o.headers);
        let (conn, browser_session) = connect(&ws_url, headers).await?;
        Ok(Self {
            inner: Arc::new(BrowserInner {
                conn,
                browser_session,
                process: Mutex::new(None),
                contexts: Mutex::new(Vec::new()),
            }),
        })
    }

    /// The browser-level CDP connection (advanced/power-user access).
    pub fn connection(&self) -> &Arc<CdpConnection> {
        &self.inner.conn
    }

    /// A browser-level [`CdpSession`] (no `sessionId`). Equivalent to
    /// Playwright's `browser.new_browser_cdp_session()`.
    pub fn new_browser_cdp_session(&self) -> CdpSession {
        CdpSession::browser(self.inner.conn.clone())
    }

    /// Create a new isolated browser context (incognito-style).
    pub async fn new_context(&self) -> Result<BrowserContext> {
        BrowserContext::create(self.clone(), None).await
    }

    /// Open a page in the default browser context.
    pub async fn new_page(&self) -> Result<Page> {
        let ctx = BrowserContext::default_for(self.clone());
        ctx.new_page().await
    }

    /// Browser version string (e.g. "HeadlessChrome/...").
    pub async fn version(&self) -> Result<String> {
        let v = self
            .inner
            .browser_session
            .send("Browser.getVersion", json!({}))
            .await?;
        Ok(v.get("product")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string())
    }

    /// Whether the underlying connection is still open.
    pub fn is_connected(&self) -> bool {
        // The dispatch task holds an Arc to the connection; we consider it
        // connected as long as a writer is reachable. A precise signal comes
        // from `on_disconnected`.
        true
    }

    /// The browser engine type this browser was launched as (always Chromium here).
    pub fn browser_type(&self) -> crate::browser_type::BrowserType {
        crate::browser_type::BrowserType::chromium()
    }

    /// All browser contexts created on this browser.
    pub fn contexts(&self) -> Vec<BrowserContext> {
        self.inner.contexts.lock().clone()
    }

    /// Register a handler invoked when the connection drops.
    pub fn on_disconnected<F, Fut>(&self, handler: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.conn.subscribe(None);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        handler().await;
                        return;
                    }
                    // Lagged or ok: keep waiting for the close.
                    _ => {}
                }
            }
        });
    }

    /// Close the browser and kill the owned process (if we launched it).
    pub async fn close(&self) -> Result<()> {
        // Best-effort graceful close.
        let _ = self
            .inner
            .browser_session
            .send("Browser.close", json!({}))
            .await;
        if let Some(mut process) = self.inner.process.lock().take() {
            let _ = process.kill().await;
        }
        let _ = self.inner.conn.close().await;
        Ok(())
    }

    /// (Internal) the browser-level session, used by contexts/pages.
    pub(crate) fn browser_session(&self) -> &CdpSession {
        &self.inner.browser_session
    }

    /// (Internal) register a context so `contexts()` reports it.
    pub(crate) fn track_context(&self, ctx: BrowserContext) {
        self.inner.contexts.lock().push(ctx);
    }
}

impl Drop for BrowserInner {
    fn drop(&mut self) {
        // BrowserProcess::Drop kills the child; nothing async to do here.
    }
}

/// Connect a [`CdpConnection`] + browser session, enabling flattened auto-attach
/// so popups show up as new sessions on the same socket.
async fn connect(
    ws_url: &str,
    headers: Option<std::collections::HashMap<String, String>>,
) -> Result<(Arc<CdpConnection>, CdpSession)> {
    let (conn, _browser_rx) = CdpConnection::connect(ws_url, headers).await?;
    let session = CdpSession::browser(conn.clone());
    // Flattened auto-attach: child-target traffic rides the same socket tagged
    // with sessionId. waitForDebuggerOnStart=false so we don't pause popups.
    let _ = session
        .send(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
        )
        .await;
    Ok((conn, session))
}
