//! `Page` — a browser tab. Tracks its main execution context, drives
//! navigation, evaluation, screenshots, and produces locators.

use crate::accessibility::Accessibility;
use crate::browser::Browser;
use crate::api_request::APIRequestContext;
use crate::cdp::session::CdpSession;
use crate::clock::Clock;
use crate::coverage::Coverage;
use crate::js_handle::JSHandle;
use crate::video::Video;
use crate::web_socket::{WebSocket, WebSocketRegistry};
use crate::web_storage::WebStorage;
use crate::download::{Download, DownloadState, DownloadStateCell};
use crate::element_handle::ElementHandle;
use crate::error::{Error, Result};
use crate::file_chooser::FileChooser;
use crate::frame::Frame;
use crate::frame_locator::FrameLocator;
use crate::keyboard::Keyboard;
use crate::locator::Locator;
use crate::mouse::Mouse;
use crate::network::{network_tracker, NetworkStore, RequestHandler, ResponseHandler};
use crate::options::{
    DragToOptions, EmulateMediaOptions, GotoOptions, ScreenshotOptions, WaitForFunctionOptions, WaitUntil,
};
use crate::request::Request;
use crate::response::Response;
use crate::route::{route_listener, Route, RouteEntry};
use crate::selectors;
use crate::touchscreen::Touchscreen;
use crate::types::{AriaRole, ConsoleMessage, Headers, Viewport};
use crate::worker::Worker;
use base64::Engine;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// A page (tab) in a browser.
#[derive(Clone)]
pub struct Page {
    inner: Arc<PageInner>,
}

type CloseHandler =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// An erased, callable binding: takes the JS args and returns a JSON value.
type ExposedBindingHandler =
    Arc<dyn Fn(Vec<Value>) -> std::pin::Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync>;

struct PageInner {
    browser: Browser,
    session: Arc<CdpSession>,
    target_id: String,
    main_world_ctx: Arc<Mutex<Option<i64>>>,
    /// Per-frame main-world execution contexts: `frame_id → context id`.
    /// Populated by the `context_tracker` from
    /// `Runtime.executionContextCreated { auxData.frameId }`. Used to scope
    /// `Frame` evaluate/locator resolution to a child frame's own context.
    frame_contexts: Arc<Mutex<HashMap<String, i64>>>,
    default_timeout_ms: AtomicU64,
    default_navigation_timeout_ms: AtomicU64,
    viewport: Mutex<Option<Viewport>>,
    mouse_pos: Arc<Mutex<(f64, f64)>>,
    network_store: Arc<NetworkStore>,
    on_request_handlers: Arc<Mutex<Vec<RequestHandler>>>,
    on_response_handlers: Arc<Mutex<Vec<ResponseHandler>>>,
    on_requestfailed_handlers: Arc<Mutex<Vec<RequestHandler>>>,
    frames: Arc<Mutex<HashMap<String, FrameData>>>,
    main_frame_id: Arc<Mutex<Option<String>>>,
    route_handlers: Arc<Mutex<Vec<RouteEntry>>>,
    route_started: AtomicBool,
    on_close_handlers: Mutex<Vec<CloseHandler>>,
    closed: Mutex<bool>,
    // Download capture: the temp dir (kept alive for the page's lifetime), its
    // path, per-guid progress state, and a one-shot flag for behavior setup.
    download_dir: Mutex<Option<Arc<tempfile::TempDir>>>,
    download_path: Mutex<Option<PathBuf>>,
    download_states: Arc<Mutex<HashMap<String, DownloadStateCell>>>,
    download_started: AtomicBool,
    // File-chooser interception: a one-shot flag for enabling interception.
    filechooser_started: AtomicBool,
    // Worker capture: a one-shot flag for enabling page-session auto-attach.
    worker_started: AtomicBool,
    // Touchscreen: a one-shot flag for enabling touch emulation. Arc-wrapped so
    // it can be shared across `Touchscreen` clones produced from this page.
    touch_emulation_started: Arc<AtomicBool>,
    // WebSocket capture: a registry that subscribes to the page session and
    // accumulates one `WebSocket` per `Network.webSocket*` connection. Attached
    // in `Page::attach` BEFORE `Network.enable` so no event is missed.
    web_socket_registry: Arc<WebSocketRegistry>,
    // Optional pre-configured video output path (Playwright configures the video
    // path up front at context/page creation time).
    video_path: Mutex<Option<PathBuf>>,
}

/// Cached frame-tree data for one frame.
#[derive(Clone)]
pub(crate) struct FrameData {
    pub url: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub detached: bool,
}

impl Page {
    /// Attach to an existing target by session/target id. Enables domains,
    /// injects the selector engine, and applies context defaults.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn attach(
        browser: Browser,
        session_id: String,
        target_id: String,
        init_scripts: &[String],
        default_timeout_ms: u64,
        extra_headers: Option<&Headers>,
        user_agent: Option<&str>,
        viewport: Option<Viewport>,
    ) -> Result<Page> {
        let session = Arc::new(CdpSession::target(
            browser.connection().clone(),
            session_id,
        ));
        session.set_default_timeout_ms(default_timeout_ms);

        let main_world_ctx: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
        let frame_contexts: Arc<Mutex<HashMap<String, i64>>> =
            Arc::new(Mutex::new(HashMap::new()));
        // The main frame id and frame store are shared with the context tracker
        // (so it can tell the main frame's default context apart from a child
        // frame's) and the frame tracker.
        let frames_store: Arc<Mutex<HashMap<String, FrameData>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let main_frame_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let ctx_rx = session.subscribe();
        // Context-tracking + engine re-injection task. Subscribe before
        // enabling Runtime so the initial executionContextCreated is captured.
        {
            let session_for_task = Arc::clone(&session);
            let ctx_cell = Arc::clone(&main_world_ctx);
            let frame_ctx_cell = Arc::clone(&frame_contexts);
            let main_id_cell = Arc::clone(&main_frame_id);
            tokio::spawn(async move {
                context_tracker(
                    ctx_rx,
                    session_for_task,
                    ctx_cell,
                    frame_ctx_cell,
                    main_id_cell,
                )
                .await;
            });
        }

        let network_store = NetworkStore::new();
        // Handler lists shared between the page and the network tracker task.
        let on_request_handlers: Arc<Mutex<Vec<RequestHandler>>> = Arc::new(Mutex::new(Vec::new()));
        let on_response_handlers: Arc<Mutex<Vec<ResponseHandler>>> =
            Arc::new(Mutex::new(Vec::new()));
        let on_requestfailed_handlers: Arc<Mutex<Vec<RequestHandler>>> =
            Arc::new(Mutex::new(Vec::new()));

        // Network capture task. Subscribe before Network.enable so no event is missed.
        {
            let net_rx = session.subscribe();
            let session_for_task = Arc::clone(&session);
            let store_for_task = Arc::clone(&network_store);
            let on_request = Arc::clone(&on_request_handlers);
            let on_response = Arc::clone(&on_response_handlers);
            let on_failed = Arc::clone(&on_requestfailed_handlers);
            tokio::spawn(async move {
                network_tracker(
                    net_rx,
                    session_for_task,
                    store_for_task,
                    on_request,
                    on_response,
                    on_failed,
                )
                .await;
            });
        }

        let page = Page {
            inner: Arc::new(PageInner {
                browser,
                session: Arc::clone(&session),
                target_id,
                main_world_ctx: Arc::clone(&main_world_ctx),
                frame_contexts: Arc::clone(&frame_contexts),
                default_timeout_ms: AtomicU64::new(default_timeout_ms),
                default_navigation_timeout_ms: AtomicU64::new(default_timeout_ms),
                viewport: Mutex::new(viewport),
                mouse_pos: Arc::new(Mutex::new((0.0, 0.0))),
                network_store,
                on_request_handlers,
                on_response_handlers,
                on_requestfailed_handlers,
                frames: Arc::clone(&frames_store),
                main_frame_id: Arc::clone(&main_frame_id),
                route_handlers: Arc::new(Mutex::new(Vec::new())),
                route_started: AtomicBool::new(false),
                on_close_handlers: Mutex::new(Vec::new()),
                closed: Mutex::new(false),
                download_dir: Mutex::new(None),
                download_path: Mutex::new(None),
                download_states: Arc::new(Mutex::new(HashMap::new())),
                download_started: AtomicBool::new(false),
                filechooser_started: AtomicBool::new(false),
                worker_started: AtomicBool::new(false),
                touch_emulation_started: Arc::new(AtomicBool::new(false)),
                web_socket_registry: Arc::new(WebSocketRegistry::new()),
                video_path: Mutex::new(None),
            }),
        };

        // WebSocket capture: attach the registry's subscriber BEFORE
        // Network.enable is sent so no `Network.webSocket*` event is missed.
        page.inner.web_socket_registry.attach(&page.inner.session);

        // Live frame-tree tracking (urls/names/children) for `Frame`.
        {
            let rx = session.subscribe();
            let frames_store = Arc::clone(&page.inner.frames);
            let main_id = Arc::clone(&page.inner.main_frame_id);
            tokio::spawn(async move {
                frame_tracker(rx, frames_store, main_id).await;
            });
        }
        // Populate the frame tree eagerly so `main_frame()` is always valid.
        let _ = page.refresh_frame_tree().await;

        // Domain enablement. Order matters only in that Runtime must be last
        // (after the context receiver exists) so contexts are captured.
        let _ = session.send("Page.enable", json!({})).await;
        let _ = session.send("Runtime.enable", json!({})).await;
        let _ = session.send("Network.enable", json!({})).await;
        let _ = session.send("Log.enable", json!({})).await;

        // Inject selector engine + user init scripts for all future documents.
        let mut source = String::from(selectors::INJECTED_SCRIPT);
        for s in init_scripts {
            source.push_str("\n");
            source.push_str(s);
        }
        let _ = session
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": source }),
            )
            .await;

        // Ensure the engine is present in whatever context exists right now.
        page.ensure_engine_in_current_context().await;

        if let Some(headers) = extra_headers {
            let _ = page.set_extra_http_headers(headers.clone()).await;
        }
        if let Some(ua) = user_agent {
            let _ = session
                .send("Emulation.setUserAgentOverride", json!({ "userAgent": ua }))
                .await;
        }
        if let Some(vp) = page.inner.viewport.lock().as_ref().copied() {
            let _ = page.set_viewport_size(vp).await;
        }

        Ok(page)
    }

    // --- accessors ---

    pub(crate) fn session(&self) -> &CdpSession {
        &self.inner.session
    }

    /// The owning CDP session as a cloned `Arc`, for building a [`JSHandle`]
    /// (which retains its own `Arc<CdpSession>`).
    pub(crate) fn session_arc(&self) -> Arc<CdpSession> {
        Arc::clone(&self.inner.session)
    }

    pub(crate) fn context_id(&self) -> Option<i64> {
        self.inner.main_world_ctx.lock().as_ref().copied()
    }

    /// The execution context to use for page-level evaluation.
    ///
    /// Returns `None` intentionally: omitting `contextId` from
    /// `Runtime.evaluate` lets CDP resolve the page's default (main-world)
    /// context itself, which is always fresh and avoids the stale-context race
    /// that occurs right after a navigation (when the old context is destroyed
    /// before the new one is reported and our tracker has not caught up).
    ///
    /// The tracked id ([`context_id`](Self::context_id)) is still maintained
    /// for callers that need a concrete handle (e.g. [`JSHandle`] context
    /// tagging) and to seed per-frame lookups.
    pub(crate) async fn ctx(&self) -> Option<i64> {
        None
    }

    /// The execution-context id to use when targeting `frame_id`.
    ///
    /// Returns `None` for the page's main frame (so its ops run in CDP's
    /// default, race-free context) and `Some(child_ctx)` for a child frame's
    /// own context. If a child frame's context has not been reported yet,
    /// returns `None` as a safe fallback (default context).
    pub(crate) fn context_for_frame(&self, frame_id: &str) -> Option<i64> {
        // The main frame uses the default context (no id) to stay race-free.
        if self
            .inner
            .main_frame_id
            .lock()
            .as_deref()
            .map(|m| m == frame_id)
            .unwrap_or(false)
        {
            return None;
        }
        self.inner
            .frame_contexts
            .lock()
            .get(frame_id)
            .copied()
            // Unknown child frame: fall back to the default context.
            .or(None)
    }

    /// Like [`context_for_frame`](Self::context_for_frame), but waits briefly
    /// for a child frame's context to arrive (the context-tracker is
    /// asynchronous and may lag a freshly-attached frame). The main frame
    /// returns `None` immediately.
    pub(crate) async fn ctx_for_frame(&self, frame_id: &str) -> Option<i64> {
        // Main frame: default context, no waiting.
        if self
            .inner
            .main_frame_id
            .lock()
            .as_deref()
            .map(|m| m == frame_id)
            .unwrap_or(false)
        {
            return None;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(2000);
        loop {
            if let Some(id) = self.inner.frame_contexts.lock().get(frame_id).copied() {
                return Some(id);
            }
            if tokio::time::Instant::now() >= deadline {
                // Unknown child frame: fall back to the default context rather
                // than erroring.
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn browser(&self) -> Browser {
        self.inner.browser.clone()
    }

    pub fn target_id(&self) -> &str {
        &self.inner.target_id
    }

    pub fn is_closed(&self) -> bool {
        *self.inner.closed.lock()
    }

    /// Set the default timeout (ms) for all actions on this page, mirroring
    /// Playwright's `page.setDefaultTimeout`. The value is stored on the page
    /// ([`PageInner::default_timeout_ms`]) and propagated to the underlying
    /// CDP session's command timeout.
    pub fn set_default_timeout(&self, ms: u64) {
        self.inner.default_timeout_ms.store(ms, Ordering::Relaxed);
        self.inner.session.set_default_timeout_ms(ms);
    }

    pub(crate) fn default_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.default_timeout_ms.load(Ordering::Relaxed))
    }

    pub(crate) fn default_navigation_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.default_navigation_timeout_ms.load(Ordering::Relaxed))
    }

    async fn ensure_engine_in_current_context(&self) {
        // Idempotent: the bundle early-returns if already installed.
        let _ = selectors::eval_context(
            &self.inner.session,
            self.ctx().await,
            "(arg) => { void arg; }",
            Value::Null,
        )
        .await;
    }

    // --- navigation ---

    /// Navigate to `url`.
    pub async fn goto(
        &self,
        url: &str,
        opts: Option<GotoOptions>,
    ) -> Result<Option<Response>> {
        let opts = opts.unwrap_or_default();
        let wait = opts.wait_until_or_default();

        // Subscribe before navigating so lifecycle events aren't missed.
        let mut rx = self.inner.session.subscribe();
        let mut params = json!({ "url": url });
        if let Some(referer) = &opts.referer {
            params["referer"] = json!(referer);
        }
        let nav = self.inner.session.send("Page.navigate", params).await?;
        if let Some(err) = nav.get("errorText").and_then(|v| v.as_str()) {
            return Err(Error::ProtocolError(format!("navigation to {url} failed: {err}")));
        }
        let loader_id = nav.get("loaderId").and_then(|v| v.as_str()).map(String::from);

        let timeout = opts.timeout.unwrap_or_else(|| self.default_timeout());
        self.wait_lifecycle(&mut rx, loader_id.as_deref(), wait, timeout, url)
            .await?;

        // Correlate the main-frame document response for this navigation.
        let response = match &loader_id {
            Some(lid) => self.wait_for_nav_response(lid).await,
            None => None,
        };
        Ok(response)
    }

    /// Poll the network store briefly for the document response of `loader_id`.
    async fn wait_for_nav_response(&self, loader_id: &str) -> Option<Response> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(r) = self.inner.network_store.response_for_loader(loader_id) {
                return Some(r);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Wait until the in-flight request count stays at 0 for ~500ms (or deadline).
    async fn wait_network_idle(&self, deadline: tokio::time::Instant) {
        let window = Duration::from_millis(500);
        loop {
            if self.inner.network_store.inflight() == 0 {
                let settled_at = tokio::time::Instant::now();
                loop {
                    if self.inner.network_store.inflight() != 0 {
                        break; // a new request started; restart
                    }
                    if tokio::time::Instant::now().duration_since(settled_at) >= window {
                        return; // stayed idle for the window
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return; // overall nav timeout
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_lifecycle(
        &self,
        rx: &mut broadcast::Receiver<crate::cdp::CdpEvent>,
        loader_id: Option<&str>,
        wait: WaitUntil,
        timeout: Duration,
        url: &str,
    ) -> Result<()> {
        if matches!(wait, WaitUntil::Commit) {
            return Ok(()); // Page.navigate returned => committed.
        }
        let target_name = match wait {
            WaitUntil::Load => "load",
            WaitUntil::DomContentLoaded => "DOMContentLoaded",
            WaitUntil::NetworkIdle => "load", // then settle below
            WaitUntil::Commit => "load",
        };

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Err(_) => {
                    return Err(Error::NavigationTimeout {
                        url: url.to_string(),
                        duration_ms: timeout.as_millis() as u64,
                    })
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(Error::ChannelClosed);
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Ok(ev)) => {
                    // Some Chrome builds emit modern `Page.lifecycleEvent`
                    // (carrying `name`), others emit the legacy discrete events
                    // (`Page.loadEventFired`, `Page.domContentEventFired`).
                    // Handle both.
                    let matched = match ev.method.as_str() {
                        "Page.lifecycleEvent" => {
                            let same_loader = loader_id
                                .map(|l| {
                                    ev.params.get("loaderId").and_then(|v| v.as_str()) == Some(l)
                                })
                                .unwrap_or(true);
                            let name = ev.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            same_loader && name == target_name
                        }
                        "Page.loadEventFired" => target_name == "load",
                        "Page.domContentEventFired" => target_name == "DOMContentLoaded",
                        _ => false,
                    };
                    if matched {
                        if matches!(wait, WaitUntil::NetworkIdle) {
                            // Settle: wait until in-flight requests hit 0 and stay there
                            // for ~500ms (Playwright's networkidle window).
                            self.wait_network_idle(deadline).await;
                        }
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Reload the page.
    pub async fn reload(&self, opts: Option<GotoOptions>) -> Result<Option<Response>> {
        let opts = opts.unwrap_or_default();
        let wait = opts.wait_until_or_default();
        let mut rx = self.inner.session.subscribe();
        let _ = self.inner.session.send("Page.reload", json!({})).await?;
        let timeout = opts.timeout.unwrap_or_else(|| self.default_timeout());
        self.wait_lifecycle(&mut rx, None, wait, timeout, "reload").await?;
        Ok(None)
    }

    /// Wait for a given document lifecycle state.
    pub async fn wait_for_load_state(&self, state: Option<WaitUntil>) -> Result<()> {
        let state = state.unwrap_or_default();
        let mut rx = self.inner.session.subscribe();
        self.wait_lifecycle(&mut rx, None, state, self.default_timeout(), "wait_for_load_state")
            .await
    }

    /// Navigate one entry back in history (best-effort; returns no response).
    pub async fn go_back(&self, opts: Option<GotoOptions>) -> Result<Option<Response>> {
        self.history_nav("history.back()", opts).await
    }

    /// Navigate one entry forward in history (best-effort; returns no response).
    pub async fn go_forward(&self, opts: Option<GotoOptions>) -> Result<Option<Response>> {
        self.history_nav("history.forward()", opts).await
    }

    async fn history_nav(
        &self,
        expr: &str,
        opts: Option<GotoOptions>,
    ) -> Result<Option<Response>> {
        let opts = opts.unwrap_or_default();
        // `history.back()/forward()` returns true iff a navigation occurred.
        let navigated: bool = self.evaluate(expr).await.unwrap_or(false);
        if navigated {
            let wait = opts.wait_until_or_default();
            let mut rx = self.inner.session.subscribe();
            let timeout = opts
                .timeout
                .unwrap_or_else(|| self.default_navigation_timeout());
            let _ = self
                .wait_lifecycle(&mut rx, None, wait, timeout, "history navigation")
                .await;
        }
        Ok(None)
    }

    /// Wait until the page URL matches `url` (exact, or `*` glob), then optionally
    /// wait for a lifecycle state.
    pub async fn wait_for_url(&self, url: &str, opts: Option<GotoOptions>) -> Result<()> {
        let opts = opts.unwrap_or_default();
        let timeout = opts
            .timeout
            .unwrap_or_else(|| self.default_navigation_timeout());
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let cur = self.url().await.unwrap_or_default();
            if glob_matches(url, &cur) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "wait_for_url '{url}' timed out after {}ms (last: '{cur}')",
                    timeout.as_millis()
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Some(state) = opts.wait_until {
            self.wait_for_load_state(Some(state)).await?;
        }
        Ok(())
    }

    /// Set the default navigation timeout (ms), separate from action timeout.
    pub fn set_default_navigation_timeout(&self, ms: u64) {
        self.inner
            .default_navigation_timeout_ms
            .store(ms, Ordering::Relaxed);
    }

    /// Emulate CSS media (media / color-scheme / reduced-motion).
    pub async fn emulate_media(&self, opts: Option<EmulateMediaOptions>) -> Result<()> {
        use crate::options::{ColorScheme, Media, ReducedMotion};
        let opts = opts.unwrap_or_default();
        let mut params = json!({});
        if let Some(m) = opts.media {
            params["media"] = json!(match m {
                Media::Screen => "screen",
                Media::Print => "print",
            });
        }
        let mut features = Vec::new();
        if let Some(cs) = opts.color_scheme {
            features.push(json!({
                "name": "prefers-color-scheme",
                "value": match cs {
                    ColorScheme::Light => "light",
                    ColorScheme::Dark => "dark",
                    ColorScheme::NoPreference => "no-preference",
                }
            }));
        }
        if let Some(rm) = opts.reduced_motion {
            features.push(json!({
                "name": "prefers-reduced-motion",
                "value": match rm {
                    ReducedMotion::Reduce => "reduce",
                    ReducedMotion::NoPreference => "no-preference",
                }
            }));
        }
        if !features.is_empty() {
            params["features"] = json!(features);
        }
        self.inner
            .session
            .send("Emulation.setEmulatedMedia", params)
            .await
            .map(|_: Value| ())
    }

    /// Toggle network offline emulation.
    pub async fn set_offline(&self, offline: bool) -> Result<()> {
        let params = if offline {
            json!({ "offline": true, "latency": 0, "downloadThroughput": 0, "uploadThroughput": 0 })
        } else {
            json!({ "offline": false, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1 })
        };
        self.inner
            .session
            .send("Network.emulateNetworkConditions", params)
            .await
            .map(|_: Value| ())
    }

    /// Drag the element matched by `source` onto the element matched by `target`.
    pub async fn drag_and_drop(
        &self,
        source: &str,
        target: &str,
        options: Option<DragToOptions>,
    ) -> Result<()> {
        let src = self.locator(source);
        let tgt = self.locator(target);
        src.drag_to(&tgt, options).await
    }

    // --- content & evaluation ---

    pub async fn url(&self) -> Result<String> {
        self.evaluate::<String>("location.href").await
    }

    pub async fn title(&self) -> Result<String> {
        self.evaluate::<String>("document.title").await
    }

    pub async fn content(&self) -> Result<String> {
        self.evaluate::<String>("document.documentElement.outerHTML").await
    }

    pub async fn set_content(&self, html: &str) -> Result<()> {
        let _ = selectors::eval_context(
            &self.inner.session,
            self.ctx().await,
            "(html) => { document.open(); document.write(html); document.close(); }",
            json!(html),
        )
        .await?;
        Ok(())
    }

    /// Evaluate a JS expression in the main world, returning a typed result.
    ///
    /// `expression` is wrapped as `(arg) => { return (<expression>); }`.
    pub async fn evaluate<R: DeserializeOwned>(&self, expression: &str) -> Result<R> {
        let function = format!("(arg) => {{ return ({expression}); }}");
        let v = selectors::eval_context(&self.inner.session, self.ctx().await, &function, Value::Null)
            .await?;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    /// Evaluate a JS expression in the main world, returning a [`JSHandle`] to
    /// the result (returned by reference, not by value). Mirrors Playwright's
    /// `page.evaluateHandle`.
    ///
    /// Like [`evaluate`](Self::evaluate), `expression` is wrapped as
    /// `(arg) => { return (<expression>); }` and run against the page's main
    /// world. The returned `Runtime.RemoteObjectId` is captured and wrapped in
    /// a [`JSHandle`]. Returns an error if the evaluation yields `null`/
    /// `undefined` (no remote object to reference).
    pub async fn evaluate_handle(&self, expression: &str) -> Result<JSHandle> {
        let function = format!("(arg) => {{ return ({expression}); }}");
        let object_id = selectors::eval_context_handle(
            &self.inner.session,
            self.ctx().await,
            &function,
            Value::Null,
        )
        .await?
        .ok_or_else(|| {
            Error::ProtocolError(
                "evaluate_handle returned no remote object (null/undefined result)".into(),
            )
        })?;
        Ok(JSHandle::with_context(
            Arc::clone(&self.inner.session),
            object_id,
            self.context_id(),
        ))
    }

    /// Evaluate a JS expression with a JSON-serializable argument.
    pub async fn evaluate_with_arg<R: DeserializeOwned, T: serde::Serialize>(
        &self,
        expression: &str,
        arg: &T,
    ) -> Result<R> {
        let function = format!("(arg) => {{ return ({expression}); }}");
        let arg_val = serde_json::to_value(arg)?;
        let v = selectors::eval_context(&self.inner.session, self.ctx().await, &function, arg_val)
            .await?;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    /// Poll `expression` until it evaluates to a truthy value, then return the
    /// deserialized result. Mirrors Playwright's `page.waitForFunction`.
    ///
    /// `expression` is wrapped as `(arg) => { return (<expression>); }` (the
    /// same form as [`Page::evaluate`]). A value is considered truthy unless it
    /// is `null`, `false`, `0`, or `""`. On timeout (default 30s) this returns
    /// [`Error::Timeout`]. Polls every `polling_interval` ms (default 100).
    pub async fn wait_for_function<R: DeserializeOwned>(
        &self,
        expression: &str,
        arg: Option<Value>,
        options: Option<WaitForFunctionOptions>,
    ) -> Result<R> {
        let opts = options.unwrap_or_default();
        let timeout = Duration::from_millis(opts.timeout.unwrap_or_else(|| self.default_timeout().as_millis() as f64) as u64);
        let poll = Duration::from_millis(opts.polling_interval.unwrap_or(100.0) as u64);
        let function = format!("(arg) => {{ return ({expression}); }}");
        let arg_val = arg.unwrap_or(Value::Null);

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let v =
                selectors::eval_context(&self.inner.session, self.ctx().await, &function, arg_val.clone())
                    .await?;
            if is_truthy(&v) {
                return serde_json::from_value::<R>(v).map_err(Error::from);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "wait_for_function timed out after {}ms (expression: {expression})",
                    timeout.as_millis()
                )));
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Evaluate `expression` against the first element matching `selector`.
    pub async fn eval_on_selector<R: DeserializeOwned>(
        &self,
        selector: &str,
        expression: &str,
    ) -> Result<R> {
        let object_id = self
            .resolve_strict(selector, Some(0))
            .await?
            .ok_or_else(|| Error::ElementNotFound(selector.to_string()))?;
        let function = format!("(el) => {{ return ({expression}); }}");
        let v = selectors::eval_object(&self.inner.session, &object_id, &function, Value::Null)
            .await?;
        self.release_object(&object_id).await;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    /// Evaluate `expression` against all elements matching `selector`.
    pub async fn eval_on_selector_all<R: DeserializeOwned>(
        &self,
        selector: &str,
        expression: &str,
    ) -> Result<Vec<R>> {
        let n = selectors::count(&self.inner.session, self.ctx().await, selector).await?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(oid) = selectors::element_at(&self.inner.session, self.ctx().await, selector, i).await? {
                let function = format!("(el) => {{ return ({expression}); }}");
                let v = selectors::eval_object(&self.inner.session, &oid, &function, Value::Null).await?;
                self.release_object(&oid).await;
                out.push(serde_json::from_value::<R>(v).map_err(Error::from)?);
            }
        }
        Ok(out)
    }

    async fn release_object(&self, object_id: &str) {
        let _ = self
            .inner
            .session
            .send("Runtime.releaseObject", json!({ "objectId": object_id }))
            .await;
    }

    // --- viewport / screenshot ---

    pub async fn set_viewport_size(&self, viewport: Viewport) -> Result<()> {
        *self.inner.viewport.lock() = Some(viewport);
        self.inner
            .session
            .send(
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": viewport.width,
                    "height": viewport.height,
                    "deviceScaleFactor": 1,
                    "mobile": false,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn screenshot(&self, opts: Option<ScreenshotOptions>) -> Result<Vec<u8>> {
        let opts = opts.unwrap_or_default();
        let format = match opts.r#type.unwrap_or_default() {
            crate::types::ScreenshotType::Png => "png",
            crate::types::ScreenshotType::Jpeg => "jpeg",
            crate::types::ScreenshotType::Webp => "webp",
        };
        let mut params = json!({ "format": format });
        if opts.full_page.unwrap_or(false) {
            params["captureBeyondViewport"] = json!(true);
        }
        if opts.omit_background.unwrap_or(false) && format == "png" {
            params["omitBackground"] = json!(true);
        }
        let resp = self
            .inner
            .session
            .send("Page.captureScreenshot", params)
            .await?;
        let data = resp
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::ProtocolError("screenshot missing data".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| Error::ProtocolError(format!("base64 decode: {e}")))?;
        if let Some(path) = &opts.path {
            tokio::fs::write(path, &bytes).await?;
        }
        Ok(bytes)
    }

    // --- headers / init scripts ---

    /// Inject a `<script>` tag into the page (inline `content` and/or `url`).
    pub async fn add_script_tag(
        &self,
        content: Option<&str>,
        url: Option<&str>,
    ) -> Result<()> {
        let arg = json!({ "content": content, "url": url });
        let _ = selectors::eval_context(
            &self.inner.session,
            self.ctx().await,
            "(a) => { const s = document.createElement('script'); if (a.url) s.src = a.url; if (a.content) s.textContent = a.content; document.head.appendChild(s); }",
            arg,
        )
        .await?;
        Ok(())
    }

    /// Inject a `<style>` tag into the page.
    pub async fn add_style_tag(&self, content: &str) -> Result<()> {
        let _ = selectors::eval_context(
            &self.inner.session,
            self.ctx().await,
            "(a) => { const s = document.createElement('style'); s.textContent = a; document.head.appendChild(s); }",
            json!(content),
        )
        .await?;
        Ok(())
    }

    /// Enable/disable the HTTP cache for this page.
    pub async fn set_cache_disabled(&self, disabled: bool) -> Result<()> {
        self.inner
            .session
            .send("Network.setCacheDisabled", json!({ "cacheDisabled": disabled }))
            .await
            .map(|_: Value| ())
    }

    /// Bring the page to the front (focus).
    pub async fn bring_to_front(&self) -> Result<()> {
        self.inner
            .session
            .send("Page.bringToFront", json!({}))
            .await
            .map(|_: Value| ())
    }

    /// Override geolocation `(latitude, longitude)` (or clear with `None`).
    /// Requires a granted `geolocation` permission.
    pub async fn set_geolocation(&self, geolocation: Option<(f64, f64)>) -> Result<()> {
        let params = match geolocation {
            Some((lat, lon)) => json!({ "latitude": lat, "longitude": lon }),
            None => json!({}),
        };
        self.inner
            .session
            .send("Emulation.setGeolocationOverride", params)
            .await
            .map(|_: Value| ())
    }

    /// Return a standalone HTTP client ([`APIRequestContext`]) tied to this
    /// page. The client shares no state with the browser tab — it is a direct
    /// HTTP client useful for API testing.
    ///
    /// Note: the page does not retain its `extra_http_headers` in Rust (they
    /// are applied directly to CDP), so the returned context starts with an
    /// empty default header set. Use [`BrowserContext::request`] to seed
    /// defaults from the context.
    pub fn request(&self) -> APIRequestContext {
        APIRequestContext::new(Headers::new())
    }

    pub async fn set_extra_http_headers(&self, headers: Headers) -> Result<()> {
        // CDP wants an array of {name, value}.
        let list: Vec<Value> = headers
            .iter()
            .map(|(k, v)| json!({ "name": k, "value": v }))
            .collect();
        self.inner
            .session
            .send("Network.setExtraHTTPHeaders", json!({ "headers": list }))
            .await?;
        Ok(())
    }

    pub async fn add_init_script(&self, script: &str) -> Result<()> {
        let _ = self
            .inner
            .session
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": script }),
            )
            .await;
        // Also run once in the current context.
        let _ = selectors::eval_context(
            &self.inner.session,
            self.ctx().await,
            "(s) => { (0, eval)(s); }",
            json!(script),
        )
        .await;
        Ok(())
    }

    /// Expose a Rust function to the page as `window[name]`.
    ///
    /// After this returns, page JS can call `await window.<name>(...args)` and
    /// receive the Rust `callback`'s return value (serialized as JSON). Mirrors
    /// Playwright's `page.exposeFunction`.
    ///
    /// The `callback` receives the JS arguments as a `Vec<serde_json::Value>`
    /// and returns a `serde_json::Value`. If the callback panics or its result
    /// fails to serialize, the JS promise resolves with
    /// `{ "__error": "<message>" }` rather than rejecting.
    ///
    /// # Mechanism
    /// A CDP binding is registered under the internal name `__pwcdpInvoke_<name>`
    /// via `Runtime.addBinding`. A JS wrapper turns `window[name]` into a
    /// Promise-returning function that forwards `{ id, args }` to that binding
    /// (as a JSON string). A per-registration listener handles
    /// `Runtime.bindingCalled`, invokes the callback on its own task, and
    /// resolves the Promise by evaluating against the pending-callback map.
    ///
    /// The wrapper is installed for future documents via
    /// `Page.addScriptToEvaluateOnNewDocument` and once in the current context.
    /// Bindings are per-execution-context and reset on navigation: the wrapper
    /// is re-installed on every new document, but `Runtime.addBinding` is only
    /// re-asserted lazily if needed (it persists across same-origin navigations
    /// on a live page session; call `expose_function` again after a cross-origin
    /// navigation if the binding goes missing).
    pub async fn expose_function<F, Fut>(&self, name: &str, callback: F) -> Result<()>
    where
        F: Fn(Vec<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Value> + Send + 'static,
    {
        // Wrap the typed callback in an erased `Arc<dyn Fn>` so the listener
        // task can invoke it without carrying generics.
        let handler: ExposedBindingHandler =
            Arc::new(move |args| Box::pin(callback(args)));

        // Register the CDP binding under a distinct internal name so it does not
        // clash with the Promise-returning `window[name]` wrapper.
        let binding_name = internal_binding_name(name);
        let _ = self
            .inner
            .session
            .send("Runtime.addBinding", json!({ "name": binding_name }))
            .await;

        // Install the JS wrapper for future documents...
        let wrapper = binding_wrapper_source(name);
        let _ = self
            .inner
            .session
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": wrapper }),
            )
            .await;
        // ...and once in the current context (idempotent).
        let _ = selectors::eval_context(
            &self.inner.session,
            self.ctx().await,
            "(s) => { (0, eval)(s); }",
            json!(wrapper),
        )
        .await;

        // Per-registration listener: dispatch bindingCalled -> callback -> resolve.
        let mut rx = self.inner.session.subscribe();
        let session = Arc::clone(&self.inner.session);
        let name_owned = name.to_string();
        tokio::spawn(async move {
            binding_listener(&mut rx, &session, &name_owned, &handler).await;
        });

        Ok(())
    }

    // --- locators ---

    pub fn locator(&self, selector: impl Into<String>) -> Locator {
        Locator::new(self.clone(), selector.into(), true, None)
    }

    /// Return a [`FrameLocator`] scoped to the same-origin `<iframe>` matched
    /// by `selector`. Element queries on the returned locator resolve inside
    /// the iframe's `contentDocument`.
    ///
    /// **Same-origin only.** Cross-origin iframes cannot be reached from the
    /// page's main world; their `contentDocument` reads as `null`, and queries
    /// against them will report zero matches. `srcdoc` iframes and same-origin
    /// `src` iframes are supported.
    pub fn frame_locator(&self, selector: impl Into<String>) -> FrameLocator {
        FrameLocator::new(self.clone(), selector.into())
    }

    pub fn get_by_text(&self, text: &str, _exact: bool) -> Locator {
        // Minimal engine treats `text=` as case-insensitive substring.
        self.locator(format!("text={text}"))
    }

    pub fn get_by_label(&self, text: &str) -> Locator {
        self.locator(format!("[aria-label=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_placeholder(&self, text: &str) -> Locator {
        self.locator(format!("[placeholder=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_alt_text(&self, text: &str) -> Locator {
        self.locator(format!("[alt=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_title(&self, text: &str) -> Locator {
        self.locator(format!("[title=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_test_id(&self, text: &str) -> Locator {
        self.locator(format!("[data-testid=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_role(&self, role: AriaRole, opts: Option<crate::options::GetByRoleOptions>) -> Locator {
        let opts = opts.unwrap_or_default();
        let mut sel = format!("role={}", role.as_str());
        if let Some(name) = &opts.name {
            sel.push_str(&format!("[name=\"{name}\"]"));
        }
        if opts.exact == Some(true) {
            sel.push_str("[exact=\"true\"]");
        }
        Locator::new(self.clone(), sel, true, None)
    }

    /// (Internal) resolve a selector to a single element RemoteObjectId,
    /// enforcing strict mode unless `index` was explicitly chosen.
    pub(crate) async fn resolve_strict(
        &self,
        selector: &str,
        forced_index: Option<usize>,
    ) -> Result<Option<String>> {
        let n = selectors::count(&self.inner.session, self.ctx().await, selector).await?;
        if n == 0 {
            return Ok(None);
        }
        let index = forced_index.unwrap_or_else(|| {
            // Strict callers (no forced index) pick 0; strict-violation is the
            // caller's responsibility. See Locator::resolve.
            0
        });
        selectors::element_at(&self.inner.session, self.ctx().await, selector, index).await
    }

    /// The first element matching `selector`, if any.
    pub async fn query_selector(&self, selector: &str) -> Result<Option<ElementHandle>> {
        let oid = self.resolve_strict(selector, Some(0)).await?;
        Ok(oid.map(|oid| ElementHandle::new(self.clone(), oid)))
    }

    /// All elements matching `selector`.
    pub async fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementHandle>> {
        let n = selectors::count(&self.inner.session, self.ctx().await, selector).await?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(oid) =
                selectors::element_at(&self.inner.session, self.ctx().await, selector, i).await?
            {
                out.push(ElementHandle::new(self.clone(), oid));
            }
        }
        Ok(out)
    }

    // --- input devices & frames ---

    pub fn keyboard(&self) -> Keyboard {
        Keyboard::new(self.clone())
    }

    pub fn mouse(&self) -> Mouse {
        Mouse::with_pos(self.clone(), Arc::clone(&self.inner.mouse_pos))
    }

    pub fn touchscreen(&self) -> Touchscreen {
        Touchscreen::with_flag(self.clone(), Arc::clone(&self.inner.touch_emulation_started))
    }

    // --- feature handles (wave-1 modules) ---

    /// The page video capture handle, mirroring Playwright's `page.video()`.
    ///
    /// If a video output path was configured for the page, it is pre-recorded on
    /// the handle (via [`Video::with_path`]); otherwise the handle starts with
    /// no path and one can be supplied at [`Video::start`].
    pub fn video(&self) -> Video {
        let session = Arc::clone(&self.inner.session);
        match self.inner.video_path.lock().clone() {
            Some(path) => Video::with_path(session, path),
            None => Video::new(session),
        }
    }

    /// The page accessibility-tree handle, mirroring Playwright's
    /// `page.accessibility()`.
    pub fn accessibility(&self) -> Accessibility {
        Accessibility::new(self.target_session())
    }

    /// The page code-coverage handle, mirroring Playwright's
    /// `page.coverage()`.
    pub fn coverage(&self) -> Coverage {
        Coverage::new(self.target_session())
    }

    /// The page fake-timer handle, mirroring Playwright's `page.clock()`.
    pub fn clock(&self) -> Clock {
        Clock::new(self.target_session())
    }

    /// The page's `localStorage` for its current security origin, mirroring
    /// Playwright's `page.evaluate(() => localStorage)`-backed access but
    /// speaking CDP's `DOMStorage` domain directly.
    ///
    /// The origin is derived from the current `location.href` (scheme://host
    /// with port). Returns a best-effort handle; if the URL has no parseable
    /// origin (e.g. `about:blank`), the empty string is used as the origin.
    pub async fn web_storage(&self) -> WebStorage {
        let origin = self.security_origin().await;
        WebStorage::local_storage(self.target_session(), origin)
    }

    /// The page's `localStorage` for its current security origin.
    pub async fn local_storage(&self) -> WebStorage {
        let origin = self.security_origin().await;
        WebStorage::local_storage(self.target_session(), origin)
    }

    /// The page's `sessionStorage` for its current security origin.
    pub async fn session_storage(&self) -> WebStorage {
        let origin = self.security_origin().await;
        WebStorage::session_storage(self.target_session(), origin)
    }

    /// Build a by-value [`CdpSession`] for this page's target.
    ///
    /// The wave-1 feature modules ([`Coverage`], [`Clock`], [`WebStorage`],
    /// [`Accessibility`]) own their session by value, but the page holds its
    /// session behind an `Arc`. We rebuild an equivalent target-level session
    /// (same connection + session id) and re-apply the page's default timeout
    /// so command timeouts match the page's configured value.
    fn target_session(&self) -> CdpSession {
        let conn = self.inner.session.connection().clone();
        let session_id = self
            .inner
            .session
            .session_id()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let session = CdpSession::target(conn, session_id);
        session.set_default_timeout_ms(self.inner.default_timeout_ms.load(Ordering::Relaxed));
        session
    }

    /// Derive the security origin (`scheme://host[:port]`) from the current page
    /// URL. Falls back to an empty origin for opaque URLs like `about:blank`.
    async fn security_origin(&self) -> String {
        let url = self.url().await.unwrap_or_default();
        derive_security_origin(&url)
    }

    pub fn main_frame(&self) -> Frame {
        Frame::main(self.clone())
    }

    /// All frames in the page (refreshed from the browser).
    pub async fn frames(&self) -> Result<Vec<Frame>> {
        self.refresh_frame_tree().await?;
        let ids: Vec<String> = self.inner.frames.lock().keys().cloned().collect();
        Ok(ids.into_iter().map(|id| Frame::new(self.clone(), id)).collect())
    }

    /// Refresh the cached frame tree from `Page.getFrameTree`.
    pub(crate) async fn refresh_frame_tree(&self) -> Result<()> {
        let resp = self
            .inner
            .session
            .send("Page.getFrameTree", json!({}))
            .await?;
        if let Some(tree) = resp.get("frameTree") {
            let mut frames = self.inner.frames.lock();
            frames.clear();
            walk_frame_tree(tree, &mut frames);
            if let Some(id) = tree
                .get("frame")
                .and_then(|f| f.get("id"))
                .and_then(|v| v.as_str())
            {
                *self.inner.main_frame_id.lock() = Some(id.to_string());
            }
        }
        Ok(())
    }

    pub(crate) fn main_frame_id(&self) -> Option<String> {
        self.inner.main_frame_id.lock().clone()
    }

    pub(crate) fn frame_data(&self, frame_id: &str) -> Option<FrameData> {
        self.inner.frames.lock().get(frame_id).cloned()
    }

    pub(crate) fn frame_ids_with_parent(&self, parent: &str) -> Vec<String> {
        self.inner
            .frames
            .lock()
            .iter()
            .filter(|(_, d)| d.parent_id.as_deref() == Some(parent))
            .map(|(k, _)| k.clone())
            .collect()
    }

    // --- events ---

    pub fn on_console<F, Fut>(&self, handler: F)
    where
        F: Fn(ConsoleMessage) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Runtime.consoleAPICalled" {
                    let msg = parse_console(&ev.params);
                    handler(msg).await;
                }
            }
        });
    }

    /// Register a handler invoked when the page fires its `load` event
    /// (`Page.loadEventFired`), mirroring Playwright's `page.on('load')`.
    pub fn on_load<F, Fut>(&self, handler: F)
    where
        F: Fn(()) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Page.loadEventFired" {
                    handler(()).await;
                }
            }
        });
    }

    /// Register a handler invoked when the page crashes (`Inspector.targetCrashed`),
    /// mirroring Playwright's `page.on('crash')`.
    pub fn on_crash<F, Fut>(&self, handler: F)
    where
        F: Fn(()) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Inspector.targetCrashed" {
                    handler(()).await;
                }
            }
        });
    }

    /// Register a handler invoked when a frame is attached to the page
    /// (`Page.frameAttached`), mirroring Playwright's `page.on('frameattached')`.
    pub fn on_frameattached<F, Fut>(&self, handler: F)
    where
        F: Fn(FrameEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Page.frameAttached" {
                    if let Some(fe) = FrameEvent::from_attached(&ev.params) {
                        handler(fe).await;
                    }
                }
            }
        });
    }

    /// Register a handler invoked when a frame is detached from the page
    /// (`Page.frameDetached`), mirroring Playwright's `page.on('framedetached')`.
    pub fn on_framedetached<F, Fut>(&self, handler: F)
    where
        F: Fn(FrameEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Page.frameDetached" {
                    if let Some(fe) = FrameEvent::from_detached(&ev.params) {
                        handler(fe).await;
                    }
                }
            }
        });
    }

    /// Register a handler invoked when a frame is navigated to a new URL
    /// (`Page.frameNavigated`), mirroring Playwright's `page.on('framenavigated')`.
    pub fn on_framenavigated<F, Fut>(&self, handler: F)
    where
        F: Fn(FrameEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Page.frameNavigated" {
                    if let Some(fe) = FrameEvent::from_navigated(&ev.params) {
                        handler(fe).await;
                    }
                }
            }
        });
    }

    pub fn on_dialog<F, Fut>(&self, handler: F)
    where
        F: Fn(Dialog) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        let session = Arc::clone(&self.inner.session);
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Page.javascriptDialogOpening" {
                    let dialog = Dialog::from_event(&ev.params, &session);
                    handler(dialog).await;
                }
            }
        });
    }

    /// Register a handler invoked for each network request sent.
    pub fn on_request<F, Fut>(&self, handler: F)
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: RequestHandler = Arc::new(move |r| Box::pin(handler(r)));
        self.inner.on_request_handlers.lock().push(h);
    }

    /// Register a handler invoked for each network response received.
    pub fn on_response<F, Fut>(&self, handler: F)
    where
        F: Fn(Response) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: ResponseHandler = Arc::new(move |r| Box::pin(handler(r)));
        self.inner.on_response_handlers.lock().push(h);
    }

    /// Register a handler invoked when a network request fails.
    pub fn on_requestfailed<F, Fut>(&self, handler: F)
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: RequestHandler = Arc::new(move |r| Box::pin(handler(r)));
        self.inner.on_requestfailed_handlers.lock().push(h);
    }

    /// Register a handler invoked when an uncaught page error occurs.
    pub fn on_pageerror<F, Fut>(&self, handler: F)
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Runtime.exceptionThrown" {
                    let msg = ev
                        .params
                        .get("exceptionDetails")
                        .and_then(|d| d.get("exception"))
                        .and_then(|e| e.get("description"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    handler(msg).await;
                }
            }
        });
    }

    /// Register a handler invoked for each new WebSocket connection, mirroring
    /// Playwright's `page.on('websocket')`.
    ///
    /// Capture is always on: the page's [`WebSocketRegistry`] subscribes to the
    /// session in [`Page::attach`] (before `Network.enable`), so every
    /// `Network.webSocket*` event is already being accumulated into a
    /// [`WebSocket`] handle. This method registers against the registry's
    /// created-connection bus and invokes `handler` for each newly-created
    /// connection. Connections created *before* this call are not replayed —
    /// register the handler early (before the page opens the socket) to observe
    /// every one.
    pub fn on_websocket<F, Fut>(&self, handler: F)
    where
        F: Fn(WebSocket) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.web_socket_registry.on_created();
        // Wrap in an Arc<dyn Fn> so the handler can be invoked from each
        // spawned task without being moved out of the loop.
        let handler: Arc<dyn Fn(WebSocket) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync> =
            Arc::new(move |ws| Box::pin(handler(ws)));
        tokio::spawn(async move {
            while let Ok(ws) = rx.recv().await {
                let h = Arc::clone(&handler);
                // Invoke on its own task so a slow handler doesn't stall the
                // registry's broadcast bus.
                tokio::spawn(async move {
                    h(ws).await;
                });
            }
        });
    }

    /// Register a handler invoked when a network request finishes loading.
    pub fn on_requestfinished<F, Fut>(&self, handler: F)
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.inner.session.subscribe();
        let store = Arc::clone(&self.inner.network_store);
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.method == "Network.loadingFinished" {
                    if let Some(rid) = ev.params.get("requestId").and_then(|v| v.as_str()) {
                        if let Some(req) = store.get_request(rid) {
                            handler(req).await;
                        }
                    }
                }
            }
        });
    }

    /// Register a handler invoked when the page closes.
    pub fn on_close<F, Fut>(&self, handler: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: CloseHandler = Arc::new(move || Box::pin(handler()));
        self.inner.on_close_handlers.lock().push(h);
    }

    /// Register a handler invoked for each file download (`Page.downloadWillBegin`).
    ///
    /// On first registration, downloads are routed to a per-page temp directory
    /// via `Page.setDownloadBehavior` `allow`. Progress events update the
    /// download's shared state so [`Download::path`] / [`Download::save_as`]
    /// can await completion.
    pub fn on_download<F, Fut>(&self, handler: F)
    where
        F: Fn(Download) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // Lazily set up the download behavior + listener task exactly once.
        if self
            .inner
            .download_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let dir = match tempfile::TempDir::new() {
                Ok(d) => Arc::new(d),
                Err(e) => {
                    tracing::error!("failed to create download temp dir: {e}");
                    return;
                }
            };
            let dir_path = dir.path().to_path_buf();
            *self.inner.download_dir.lock() = Some(Arc::clone(&dir));
            *self.inner.download_path.lock() = Some(dir_path.clone());

            // Tell Chrome to save downloads into our temp dir.
            let session_for_setup = Arc::clone(&self.inner.session);
            let setup_path = dir_path.clone();
            tokio::spawn(async move {
                let _ = session_for_setup
                    .send(
                        "Page.setDownloadBehavior",
                        json!({ "behavior": "allow", "downloadPath": setup_path }),
                    )
                    .await;
            });

            // Listener task: maps downloadWillBegin -> Download, downloadProgress -> state.
            let mut rx = self.inner.session.subscribe();
            let session = Arc::clone(&self.inner.session);
            let states = Arc::clone(&self.inner.download_states);
            let download_path = dir_path.clone();
            // The handler is wrapped in an Arc and invoked from the task. We
            // hold a Page clone so each Download can reference its page.
            let page = self.clone();
            let handler: Arc<dyn Fn(Download) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync> =
                Arc::new(move |d| Box::pin(handler(d)));
            tokio::spawn(async move {
                download_listener(
                    &mut rx,
                    &session,
                    &states,
                    &download_path,
                    page.clone(),
                    &handler,
                )
                .await;
            });
        }
    }

    /// Register a handler invoked when a file-chooser dialog opens
    /// (`Page.fileChooserOpened`), mirroring Playwright's `page.on('filechooser')`.
    ///
    /// On first registration, chooser interception is enabled once via
    /// `Page.setInterceptFileChooserDialog { enabled: true }` (must be in place
    /// before the action that opens the chooser). The handler may inspect the
    /// [`FileChooser`] and call [`FileChooser::set_files`] to accept it.
    pub async fn on_filechooser<F, Fut>(&self, handler: F)
    where
        F: Fn(FileChooser) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // Lazily enable interception + spawn the listener task exactly once.
        if self
            .inner
            .filechooser_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            // Subscribe before enabling so the first fileChooserOpened event
            // is never missed.
            let mut rx = self.inner.session.subscribe();
            // Enable interception and await it inline so the caller knows it is
            // in place before triggering the chooser.
            let _ = self
                .inner
                .session
                .send(
                    "Page.setInterceptFileChooserDialog",
                    json!({ "enabled": true }),
                )
                .await;

            // Wrap the handler in an Arc for invocation from the task.
            let page = self.clone();
            let handler: Arc<
                dyn Fn(FileChooser) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>>
                    + Send
                    + Sync,
            > = Arc::new(move |fc| Box::pin(handler(fc)));
            tokio::spawn(async move {
                while let Ok(ev) = rx.recv().await {
                    if ev.method == "Page.fileChooserOpened" {
                        let multiple = ev
                            .params
                            .get("mode")
                            .and_then(|v| v.as_str())
                            == Some("selectMultiple");
                        let backend_node_id = ev
                            .params
                            .get("backendNodeId")
                            .and_then(|v| v.as_i64());
                        let fc = FileChooser::new(page.clone(), backend_node_id, multiple);
                        let h = Arc::clone(&handler);
                        tokio::spawn(async move {
                            (h)(fc).await;
                        });
                    }
                }
            });
        }
    }

    /// Register a handler invoked when a web/service/shared worker is created,
    /// mirroring Playwright's `page.on('worker')`.
    ///
    /// On first registration, flattened auto-attach is enabled on the page
    /// session (`Target.setAutoAttach { flatten: true }`) so workers show up as
    /// child sessions on the same connection. Each child target of type
    /// `worker`/`service_worker`/`shared_worker` surfaces via
    /// `Target.attachedToTarget` and is wrapped in a [`Worker`].
    ///
    /// Subscribe before enabling so the first `attachedToTarget` is never missed.
    pub async fn on_worker<F, Fut>(&self, handler: F)
    where
        F: Fn(Worker) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // Lazily enable page-session auto-attach + spawn the listener once.
        if self
            .inner
            .worker_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            // Subscribe BEFORE enabling auto-attach so the attach event for a
            // worker spawned right after is captured.
            let mut rx = self.inner.session.subscribe();
            let _ = self
                .inner
                .session
                .send(
                    "Target.setAutoAttach",
                    json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true }),
                )
                .await;

            let connection = self.inner.session.connection().clone();
            let handler: Arc<
                dyn Fn(Worker) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
            > = Arc::new(move |w| Box::pin(handler(w)));
            tokio::spawn(async move {
                while let Ok(ev) = rx.recv().await {
                    if ev.method != "Target.attachedToTarget" {
                        continue;
                    }
                    // Defensive casing: `sessionId` (modern) then `session_id`.
                    let session_id = ev
                        .params
                        .get("sessionId")
                        .or_else(|| ev.params.get("session_id"))
                        .and_then(|v| v.as_str());
                    let target_info = match ev.params.get("targetInfo") {
                        Some(t) => t,
                        None => continue,
                    };
                    let target_type = target_info
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // Only workers — skip iframes, pages, popups, etc.
                    if !matches!(target_type, "worker" | "service_worker" | "shared_worker") {
                        continue;
                    }
                    let (sid, url) = match (session_id, target_info.get("url").and_then(|v| v.as_str())) {
                        (Some(sid), Some(url)) => (sid.to_string(), url.to_string()),
                        _ => continue,
                    };
                    let worker = Worker::new(connection.clone(), sid, url);
                    // Enable Runtime on the worker session so evaluate works.
                    worker.enable_runtime().await;
                    let h = Arc::clone(&handler);
                    tokio::spawn(async move {
                        (h)(worker).await;
                    });
                }
            });
        }
    }

    // --- close ---

    pub async fn close(&self) -> Result<()> {
        if *self.inner.closed.lock() {
            return Ok(());
        }
        *self.inner.closed.lock() = true;
        let handlers = std::mem::take(&mut *self.inner.on_close_handlers.lock());
        for h in handlers {
            tokio::spawn(async move { (h)().await; });
        }
        let _ = self
            .inner
            .browser
            .browser_session()
            .send("Target.closeTarget", json!({ "targetId": self.inner.target_id }))
            .await;
        Ok(())
    }

    // --- network interception (Fetch domain) ---

    /// Intercept requests matching `pattern` (a URL glob, `*`-wildcarded) and
    /// route them to `handler`. The handler must continue/fulfill/abort the
    /// [`Route`] (an unhandled route stalls the request).
    pub async fn route<F, Fut>(&self, pattern: &str, handler: F) -> Result<()>
    where
        F: Fn(Route) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let entry = RouteEntry {
            pattern: pattern.to_string(),
            handler: Arc::new(move |r| Box::pin(handler(r))),
        };
        self.inner.route_handlers.lock().push(entry);
        self.ensure_route_listener().await;
        self.refresh_fetch_patterns().await
    }

    /// Remove the first route registered with exactly `pattern`.
    pub async fn unroute(&self, pattern: &str) -> Result<()> {
        let mut handlers = self.inner.route_handlers.lock();
        if let Some(pos) = handlers.iter().position(|e| e.pattern == pattern) {
            handlers.remove(pos);
        }
        drop(handlers);
        self.refresh_fetch_patterns().await
    }

    /// Remove all routes.
    pub async fn unroute_all(&self) -> Result<()> {
        self.inner.route_handlers.lock().clear();
        let _ = self.inner.session.send("Fetch.disable", json!({})).await;
        Ok(())
    }

    async fn ensure_route_listener(&self) {
        if self
            .inner
            .route_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let rx = self.inner.session.subscribe();
            let session = Arc::clone(&self.inner.session);
            let store = Arc::clone(&self.inner.network_store);
            let handlers = Arc::clone(&self.inner.route_handlers);
            tokio::spawn(async move {
                route_listener(rx, session, store, handlers).await;
            });
        }
    }

    async fn refresh_fetch_patterns(&self) -> Result<()> {
        let patterns: Vec<Value> = self
            .inner
            .route_handlers
            .lock()
            .iter()
            .map(|e| json!({ "urlPattern": e.pattern }))
            .collect();
        if patterns.is_empty() {
            let _ = self.inner.session.send("Fetch.disable", json!({})).await;
        } else {
            self.inner
                .session
                .send("Fetch.enable", json!({ "patterns": patterns }))
                .await?;
        }
        Ok(())
    }

    // --- Tier 3 stubs (API completeness) ---

    pub async fn pdf(&self, opts: Option<crate::options::PdfOptions>) -> Result<Vec<u8>> {
        let opts = opts.unwrap_or_default();
        let mut params = json!({ "printBackground": opts.print_background.unwrap_or(true) });
        if let Some(fmt) = opts.format {
            let (w, h) = fmt.inches();
            params["paperWidth"] = json!(w);
            params["paperHeight"] = json!(h);
        }
        if let Some(l) = opts.landscape {
            params["landscape"] = json!(l);
        }
        if let Some(s) = opts.scale {
            params["scale"] = json!(s);
        }
        if let Some(p) = opts.prefer_css_page_size {
            params["preferCSSPageSize"] = json!(p);
        }
        if let Some(m) = opts.margin {
            if let Some(v) = m.top {
                params["marginTop"] = json!(v);
            }
            if let Some(v) = m.bottom {
                params["marginBottom"] = json!(v);
            }
            if let Some(v) = m.left {
                params["marginLeft"] = json!(v);
            }
            if let Some(v) = m.right {
                params["marginRight"] = json!(v);
            }
        }
        let resp = self.inner.session.send("Page.printToPDF", params).await?;
        let data = resp
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::ProtocolError("pdf missing data".into()))?;
        Ok(base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| Error::ProtocolError(format!("pdf base64 decode: {e}")))?)
    }

    /// Capture an aria-snapshot (Playwright's YAML-ish accessibility-tree
    /// format) of the whole page.
    ///
    /// Fetches the full accessibility tree via `Accessibility.getFullAXTree`
    /// and serializes it with [`crate::aria_snapshot`]. The Accessibility
    /// domain is enabled best-effort first (some Chrome builds require it).
    pub async fn aria_snapshot(&self) -> Result<String> {
        let _ = self
            .inner
            .session
            .send("Accessibility.enable", json!({}))
            .await;
        let resp = self
            .inner
            .session
            .send("Accessibility.getFullAXTree", json!({}))
            .await?;
        let nodes = resp
            .get("nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(crate::aria_snapshot::serialize(&nodes, None))
    }
}

fn attr_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Derive the CDP `securityOrigin` (`scheme://host[:port]`) from a URL string.
///
/// Opaque schemes (`about:`, `data:`, `blob:`) yield an empty origin, since
/// `DOMStorage` has nothing to scope to for them. A URL without a parseable
/// scheme also yields an empty origin.
fn derive_security_origin(url: &str) -> String {
    // Find the scheme delimiter.
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => return String::new(),
    };
    let scheme = &url[..url.find("://").unwrap()];
    // Opaque/special schemes have no real origin.
    if matches!(scheme, "about" | "data" | "blob" | "javascript") {
        return String::new();
    }
    // The authority ends at the first `/`, `?`, or `#`.
    let end = after_scheme
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    if authority.is_empty() {
        return String::new();
    }
    format!("{scheme}://{authority}")
}

/// Whether a JSON value is "truthy" for `wait_for_function`: `null`, `false`,
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Match `pattern` against `value`: exact, or a leading/trailing/both `*` glob
/// (good enough for `wait_for_url`).
fn glob_matches(pattern: &str, value: &str) -> bool {
    if pattern == value {
        return true;
    }
    // `*middle*` → contains.
    if pattern.len() >= 2 && pattern.starts_with('*') && pattern.ends_with('*') {
        return value.contains(&pattern[1..pattern.len() - 1]);
    }
    if let Some(rest) = pattern.strip_prefix('*') {
        if value.ends_with(rest) {
            return true;
        }
    }
    if let Some(rest) = pattern.strip_suffix('*') {
        if value.starts_with(rest) {
            return true;
        }
    }
    false
}

/// Recursively walk a `Page.getFrameTree` response into the frame store.
fn walk_frame_tree(node: &Value, frames: &mut HashMap<String, FrameData>) {
    let frame = match node.get("frame") {
        Some(f) => f,
        None => return,
    };
    let id = match frame.get("id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let data = FrameData {
        url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        name: frame.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        parent_id: frame
            .get("parentId")
            .and_then(|v| v.as_str())
            .map(String::from),
        detached: false,
    };
    frames.insert(id, data);
    if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
        for child in children {
            walk_frame_tree(child, frames);
        }
    }
}

/// Background task: keep the frame store fresh from `Page.frame*` events.
async fn frame_tracker(
    mut rx: broadcast::Receiver<crate::cdp::CdpEvent>,
    frames: Arc<Mutex<HashMap<String, FrameData>>>,
    main_id: Arc<Mutex<Option<String>>>,
) {
    loop {
        match rx.recv().await {
            Ok(ev) => match ev.method.as_str() {
                "Page.frameNavigated" => {
                    if let Some(frame) = ev.params.get("frame") {
                        let id = match frame.get("id").and_then(|v| v.as_str()) {
                            Some(s) => s.to_string(),
                            None => continue,
                        };
                        let parent = frame
                            .get("parentId")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        let data = FrameData {
                            url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            name: frame.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            parent_id: parent.clone(),
                            detached: false,
                        };
                        if parent.is_none() {
                            *main_id.lock() = Some(id.clone());
                        }
                        frames.lock().insert(id, data);
                    }
                }
                "Page.frameDetached" => {
                    if let Some(id) = ev.params.get("frameId").and_then(|v| v.as_str()) {
                        if let Some(d) = frames.lock().get_mut(id) {
                            d.detached = true;
                        }
                    }
                }
                _ => {}
            },
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
        }
    }
}

/// Background task: dispatch `Page.downloadWillBegin`/`downloadProgress` events.
async fn download_listener(
    rx: &mut broadcast::Receiver<crate::cdp::CdpEvent>,
    _session: &Arc<CdpSession>,
    states: &Arc<Mutex<HashMap<String, DownloadStateCell>>>,
    download_path: &std::path::Path,
    page: Page,
    handler: &Arc<
        dyn Fn(Download) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
    >,
) {
    while let Ok(ev) = rx.recv().await {
        match ev.method.as_str() {
            "Page.downloadWillBegin" => {
                let guid = ev
                    .params
                    .get("guid")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let url = ev
                    .params
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let suggested = ev
                    .params
                    .get("suggestedFilename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("download")
                    .to_string();
                let cell = DownloadStateCell::new();
                states.lock().insert(guid.clone(), cell.clone());
                let dl = Download::new(
                    url,
                    suggested,
                    guid,
                    cell,
                    download_path.to_path_buf(),
                    page.clone(),
                );
                let h = Arc::clone(handler);
                tokio::spawn(async move {
                    (h)(dl).await;
                });
            }
            "Page.downloadProgress" => {
                let guid = match ev.params.get("guid").and_then(|v| v.as_str()) {
                    Some(g) => g.to_string(),
                    None => continue,
                };
                let state = ev
                    .params
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("InProgress");
                let mapped = match state {
                    "Completed" => DownloadState::Completed,
                    "Canceled" => DownloadState::Canceled,
                    _ => DownloadState::InProgress,
                };
                if let Some(cell) = states.lock().get(&guid) {
                    *cell.state.lock() = mapped;
                }
            }
            _ => {}
        }
    }
}

/// The distinct internal name under which the CDP binding for the public
/// `name` is registered (`Runtime.addBinding`). Keeping it separate avoids
/// clashing with the Promise-returning `window[name]` wrapper.
fn internal_binding_name(name: &str) -> String {
    format!("__pwcdpInvoke_{name}")
}

/// The JS wrapper that turns `window[name]` into a Promise-returning function.
/// Each call assigns a monotonic id, registers its `resolve`, and forwards
/// `{ id, args }` (as a JSON string) to the CDP binding under
/// `__pwcdpInvoke_<name>`. The Rust listener resolves the promise later.
fn binding_wrapper_source(name: &str) -> String {
    // `name` is interpolated as a JSON string literal so it is safe to embed
    // even if it contains quotes/backslashes.
    let name_json = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"(function(){{
  var publicName = {name_json};
  var invokeName = "__pwcdpInvoke_" + publicName;
  var pending = (self.__pwcdpBindings = self.__pwcdpBindings || {{}});
  var map = pending[publicName] = pending[publicName] || {{ id: 0, cbs: {{}} }};
  self[publicName] = function(){{
    var args = Array.prototype.slice.call(arguments);
    return new Promise(function(resolve){{
      var id = ++map.id;
      map.cbs[id] = resolve;
      // The CDP binding (self[invokeName]) is installed by Runtime.addBinding;
      // if it is not present yet, surface an error so callers see a clear cause.
      if (typeof self[invokeName] !== "function") {{
        delete map.cbs[id];
        resolve({{ "__error": "binding not installed: " + invokeName }});
        return;
      }}
      try {{
        self[invokeName](JSON.stringify({{ id: id, args: args }}));
      }} catch (e) {{
        delete map.cbs[id];
        resolve({{ "__error": String((e && e.message) || e) }});
      }}
    }});
  }};
}})();
"#
    )
}

/// Background task for one `expose_function` registration: on
/// `Runtime.bindingCalled` for the internal binding name, parse the payload,
/// run the Rust callback on its own task, then resolve the JS promise.
async fn binding_listener(
    rx: &mut broadcast::Receiver<crate::cdp::CdpEvent>,
    session: &Arc<CdpSession>,
    name: &str,
    handler: &ExposedBindingHandler,
) {
    let binding_name = internal_binding_name(name);
    loop {
        match rx.recv().await {
            Ok(ev) if ev.method == "Runtime.bindingCalled" => {
                let fired_name = ev
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if fired_name != binding_name {
                    continue;
                }
                let payload_str = match ev.params.get("payload").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => continue,
                };
                let parsed: Value = match serde_json::from_str(payload_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = match parsed.get("id").and_then(|v| v.as_i64()) {
                    Some(i) => i,
                    None => continue,
                };
                let args = parsed
                    .get("args")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                let h = Arc::clone(handler);
                let session = Arc::clone(session);
                let name_owned = name.to_string();
                // Resolve on a separate task so a slow callback doesn't stall
                // event processing.
                tokio::spawn(async move {
                    let result = (h)(args).await;
                    let result_json = serde_json::to_string(&result)
                        .unwrap_or_else(|_| r#"{"__error":"serialize failed"}"#.to_string());
                    // Re-serialize as a JSON string literal so it can be passed
                    // to JSON.parse safely (handles quotes/newlines/etc.).
                    let result_str_literal = serde_json::to_string(&result_json)
                        .unwrap_or_else(|_| r#""{\"__error\":\"serialize failed\"}""#.to_string());
                    // Resolve the pending promise: look up the resolve fn by id,
                    // call it with the parsed result, then delete the entry.
                    let name_j = serde_json::to_string(&name_owned).unwrap_or_else(|_| "\"\"".into());
                    let expr = format!(
                        "(function(){{
  var m = (self.__pwcdpBindings && self.__pwcdpBindings[{name_j}]) || null;
  if (!m || !m.cbs || !m.cbs[{id}]) return;
  var fn_ = m.cbs[{id}]; delete m.cbs[{id}];
  fn_(JSON.parse({result_str_literal}));
}})();"
                    );
                    let _ = session
                        .send(
                            "Runtime.evaluate",
                            json!({ "expression": expr, "awaitPromise": false }),
                        )
                        .await;
                });
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
        }
    }
}

fn parse_console(params: &Value) -> ConsoleMessage {    let text = params
        .get("args")
        .and_then(|a| a.as_array())
        .map(|args| {
            args.iter()
                .filter_map(|a| {
                    a.get("value")
                        .and_then(|v| v.as_str())
                        .or_else(|| a.get("description").and_then(|v| v.as_str()))
                        .map(String::from)
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let kind = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("log")
        .to_string();
    // Source location from the top call frame of the reported stack trace.
    let location = params
        .get("stackTrace")
        .and_then(|s| s.get("callFrames"))
        .and_then(|f| f.as_array())
        .and_then(|frames| frames.first())
        .map(|frame| {
            use crate::types::ConsoleMessageLocation;
            ConsoleMessageLocation {
                url: frame
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                line_number: frame.get("lineNumber").and_then(|v| v.as_i64()),
                column_number: frame.get("columnNumber").and_then(|v| v.as_i64()),
            }
        });
    ConsoleMessage {
        text,
        r#type: kind,
        location,
    }
}

/// Background task: track the main-world execution context and keep the
/// selector engine installed after navigations. Also records per-frame
/// contexts (frame id → context id) from `auxData.frameId` so child-frame
/// evaluation/locator resolution can target a frame's own context.
///
/// `main_world_ctx` is set ONLY for the main (top) frame's default context —
/// a child iframe also has a "default" context, and accepting every default
/// context would let the child's context clobber the main one.
async fn context_tracker(
    mut rx: broadcast::Receiver<crate::cdp::CdpEvent>,
    session: Arc<CdpSession>,
    ctx_cell: Arc<Mutex<Option<i64>>>,
    frame_ctx_cell: Arc<Mutex<HashMap<String, i64>>>,
    main_frame_id: Arc<Mutex<Option<String>>>,
) {
    loop {
        match rx.recv().await {
            Ok(ev) => match ev.method.as_str() {
                "Runtime.executionContextCreated" => {
                    let context = match ev.params.get("context") {
                        Some(c) => c,
                        None => continue,
                    };
                    let aux = context.get("auxData");
                    let is_default = aux
                        .and_then(|a| a.get("type"))
                        .and_then(|t| t.as_str())
                        == Some("default");
                    let id = context.get("id").and_then(|i| i.as_i64());
                    let Some(id) = id else { continue };

                    let frame_id = aux
                        .and_then(|a| a.get("frameId"))
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    if is_default {
                        // Record the frame → context mapping. Only default
                        // (main-world) contexts are tracked; utility/isolated
                        // worlds have their own ids and must not shadow a
                        // frame's main context.
                        if let Some(fid) = &frame_id {
                            frame_ctx_cell.lock().insert(fid.clone(), id);
                        }

                        // The page-wide "main" context is the main frame's
                        // default context. A child iframe's default context
                        // must NOT overwrite it. Accept this context as main
                        // when (a) its frame is the known main frame, or
                        // (b) the main frame id is not yet known and no main
                        // context has been recorded yet (initial top-frame
                        // context typically arrives before the frame tree is
                        // populated).
                        let main_id = main_frame_id.lock().clone();
                        let is_main_frame = match (&main_id, &frame_id) {
                            (Some(m), Some(f)) => m == f,
                            (None, _) => ctx_cell.lock().is_none(),
                            // main frame known but this context has no frameId:
                            // treat as main only if none recorded yet.
                            (Some(_), None) => ctx_cell.lock().is_none(),
                        };
                        if is_main_frame {
                            *ctx_cell.lock() = Some(id);
                        }
                    }

                    // Ensure the engine is installed in this context.
                    let s = Arc::clone(&session);
                    tokio::spawn(async move {
                        let _ = s
                            .send(
                                "Runtime.evaluate",
                                json!({
                                    "expression": selectors::INJECTED_SCRIPT,
                                    "contextId": id,
                                }),
                            )
                            .await;
                    });
                }
                "Runtime.executionContextsCleared" => {
                    *ctx_cell.lock() = None;
                    frame_ctx_cell.lock().clear();
                }
                "Runtime.executionContextDestroyed" => {
                    let id = ev
                        .params
                        .get("executionContextId")
                        .and_then(|v| v.as_i64());
                    {
                        let mut cell = ctx_cell.lock();
                        if id.is_some() && *cell == id {
                            *cell = None;
                        }
                    }
                    if let Some(id) = id {
                        let mut frames = frame_ctx_cell.lock();
                        // Drop any frame→context entries that pointed at the
                        // destroyed context (a new contextCreated will repopulate).
                        frames.retain(|_, v| *v != id);
                    }
                }
                _ => {}
            },
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
        }
    }
}

/// A frame lifecycle event payload for `on_frameattached` /
/// `on_framedetached` / `on_framenavigated`. Carries the frame id, its
/// parent's id (if any), and — for navigations — the frame's new URL.
#[derive(Debug, Clone)]
pub struct FrameEvent {
    /// The id of the frame the event concerns.
    pub id: String,
    /// The parent frame id, if the frame has one (the main frame does not).
    pub parent_id: Option<String>,
    /// The frame's URL. Populated for navigations; empty for attach/detach.
    pub url: String,
}

impl FrameEvent {
    /// Parse `Page.frameAttached` params (`params.frame.{id,parentFrameId}`).
    pub(crate) fn from_attached(params: &Value) -> Option<Self> {
        let frame = params.get("frame").unwrap_or(params);
        let id = frame.get("id").and_then(|v| v.as_str())?.to_string();
        let parent_id = frame
            .get("parentFrameId")
            .and_then(|v| v.as_str())
            .map(String::from);
        Some(Self {
            id,
            parent_id,
            url: String::new(),
        })
    }

    /// Parse `Page.frameDetached` params (`params.frameId`, plus any `frame`).
    pub(crate) fn from_detached(params: &Value) -> Option<Self> {
        // frameDetached carries a bare `frameId`; `frame` may be absent.
        let id = params
            .get("frameId")
            .or_else(|| params.get("frame"))
            .and_then(|v| {
                v.as_str()
                    .map(String::from)
                    .or_else(|| v.get("id").and_then(|i| i.as_str()).map(String::from))
            })?;
        let parent_id = params
            .get("frame")
            .and_then(|f| f.get("parentFrameId"))
            .and_then(|v| v.as_str())
            .map(String::from);
        Some(Self {
            id,
            parent_id,
            url: String::new(),
        })
    }

    /// Parse `Page.frameNavigated` params (`params.frame.{id,url,parentId}`).
    pub(crate) fn from_navigated(params: &Value) -> Option<Self> {
        let frame = params.get("frame")?;
        let id = frame.get("id").and_then(|v| v.as_str())?.to_string();
        let url = frame
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let parent_id = frame
            .get("parentId")
            .and_then(|v| v.as_str())
            .map(String::from);
        Some(Self {
            id,
            parent_id,
            url,
        })
    }
}

/// A JavaScript dialog (alert/confirm/prompt/beforeunload).
pub struct Dialog {
    message: String,
    kind: String,
    session: Arc<CdpSession>,
}

impl Dialog {
    pub(crate) fn from_event(params: &Value, session: &Arc<CdpSession>) -> Self {
        Self {
            message: params
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            kind: params
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("alert")
                .to_string(),
            session: Arc::clone(session),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub async fn accept(&self, prompt_text: Option<&str>) -> Result<()> {
        let mut p = json!({ "accept": true });
        if let Some(t) = prompt_text {
            p["promptText"] = json!(t);
        }
        self.session
            .send("Page.handleJavaScriptDialog", p)
            .await
            .map(|_| ())
    }

    pub async fn dismiss(&self) -> Result<()> {
        self.session
            .send("Page.handleJavaScriptDialog", json!({ "accept": false }))
            .await
            .map(|_| ())
    }
}
