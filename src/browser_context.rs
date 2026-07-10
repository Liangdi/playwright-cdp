//! `BrowserContext` — an isolated context (incognito-style), owning pages,
//! init scripts, cookies, and defaults.

use crate::browser::Browser;
use crate::api_request::APIRequestContext;
use crate::cdp::session::CdpSession;
use crate::download::Download;
use crate::error::{Error, Result};
use crate::options::NewContextOptions;
use crate::page::Page;
use crate::request::Request;
use crate::response::Response;
use crate::route::RouteEntry;
use crate::route::{Route, RouteFulfillOptions, RouteHandler};
use crate::types::{ConsoleMessage, Headers, Position};
use crate::clock::{Clock, ClockInstallOptions};
use crate::har::{routes_from_har, HarRoute, RouteFromHarOptions};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

type PageHandler = Arc<
    dyn Fn(Page) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

/// Erased handler for `Request`/`on_requestfailed` events.
type RequestHandler =
    Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Erased handler for `Response` events.
type ResponseHandler =
    Arc<dyn Fn(Response) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Erased handler for `ConsoleMessage` events.
type ConsoleHandler =
    Arc<dyn Fn(ConsoleMessage) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Erased handler for `Download` events.
type DownloadHandler =
    Arc<dyn Fn(Download) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Erased handler for close events (no payload).
type CloseHandler =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Erased, callable exposed-binding: takes the JS args and returns a JSON value.
type ExposedBindingHandler =
    Arc<dyn Fn(Vec<Value>) -> Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync>;

/// A named exposed function registered at the context level, applied to every
/// page (existing and future). The `Arc` handler is cloned per page.
#[derive(Clone)]
struct ExposedFunction {
    name: String,
    handler: ExposedBindingHandler,
}

/// A browser context. The default context has `browser_context_id == None`;
/// isolated contexts are created via [`Browser::new_context`](crate::Browser::new_context).
#[derive(Clone)]
pub struct BrowserContext {
    inner: Arc<ContextInner>,
}

struct ContextInner {
    browser: Browser,
    browser_context_id: Option<String>,
    init_scripts: Mutex<Vec<String>>,
    default_timeout_ms: AtomicU64,
    /// Per-page default navigation timeout (ms). Applied to existing + future
    /// pages via `Page::set_default_navigation_timeout`.
    default_navigation_timeout_ms: AtomicU64,
    extra_http_headers: Mutex<Option<Headers>>,
    user_agent: Mutex<Option<String>>,
    viewport: Mutex<Option<crate::types::Viewport>>,
    /// Geolocation override applied to existing + future pages.
    geolocation: Mutex<Option<Position>>,
    /// Offline-emulation flag applied to existing + future pages.
    offline: Mutex<bool>,
    pages: Mutex<Vec<Page>>,
    on_page_handlers: Mutex<Vec<PageHandler>>,
    /// Context-level route registry. Each entry is applied to every existing
    /// page and to pages created after the route is registered.
    context_routes: Mutex<Vec<RouteEntry>>,
    /// Context-level exposed functions, applied to existing + future pages.
    exposed_functions: Mutex<Vec<ExposedFunction>>,
    utility_page: Mutex<Option<Page>>,
    /// A single shared Tracing handle (browser-level session). `tracing()`
    /// returns clones of this so start/stop land on the same state.
    tracing: crate::Tracing,
    /// Pending fake-timer install config registered at the context level.
    /// When set, every page created via `new_page()` runs
    /// `page.clock().install(...)` so context-level fake timers apply forward.
    /// See [`BrowserContext::clock`] / [`BrowserContext::set_clock_install`].
    pending_clock_install: Mutex<Option<ClockInstallOptions>>,
    closed: Mutex<bool>,

    // --- Context-level event handler lists (fan out to every page) ---
    on_close_handlers: Mutex<Vec<CloseHandler>>,
    on_console_handlers: Mutex<Vec<ConsoleHandler>>,
    on_request_handlers: Mutex<Vec<RequestHandler>>,
    on_response_handlers: Mutex<Vec<ResponseHandler>>,
    on_requestfailed_handlers: Mutex<Vec<RequestHandler>>,
    on_download_handlers: Mutex<Vec<DownloadHandler>>,
}

/// Build an owned [`CdpSession`] that targets the given page's session.
///
/// `CdpSession` is not `Clone` and `Page::session()` returns a borrow, but
/// [`Clock`] takes a session by value. This reconstructs a target-scoped session
/// sharing the page's connection and session id — cheap (an `Arc` clone + an
/// `Option<String>` clone) and pointed at the same CDP session.
fn page_session_owned(page: &Page) -> CdpSession {
    let session = page.session();
    CdpSession::target(
        Arc::clone(session.connection()),
        session.session_id().unwrap_or(""),
    )
}

impl BrowserContext {
    /// Create a new isolated context (calls `Target.createBrowserContext`).
    pub(crate) async fn create(browser: Browser, opts: Option<NewContextOptions>) -> Result<Self> {
        let v = browser
            .browser_session()
            .send(
                "Target.createBrowserContext",
                json!({"disposeOnDetach": true}),
            )
            .await?;
        let bcid = v
            .get("browserContextId")
            .and_then(|x| x.as_str())
            .ok_or_else(|| Error::ProtocolError("createBrowserContext missing id".into()))?
            .to_string();

        let ctx = Self::build(browser.clone(), Some(bcid), opts);
        browser.track_context(ctx.clone());
        Ok(ctx)
    }

    /// Wrap the default browser context (no create call).
    pub(crate) fn default_for(browser: Browser) -> Self {
        Self::build(browser, None, None)
    }

    fn build(browser: Browser, bcid: Option<String>, opts: Option<NewContextOptions>) -> Self {
        let opts = opts.unwrap_or_default();
        let tracing = crate::Tracing::new(browser.new_browser_cdp_session());
        Self {
            inner: Arc::new(ContextInner {
                browser,
                browser_context_id: bcid,
                init_scripts: Mutex::new(Vec::new()),
                default_timeout_ms: AtomicU64::new(30_000),
                default_navigation_timeout_ms: AtomicU64::new(30_000),
                extra_http_headers: Mutex::new(opts.extra_http_headers),
                user_agent: Mutex::new(opts.user_agent),
                viewport: Mutex::new(opts.viewport),
                geolocation: Mutex::new(None),
                offline: Mutex::new(false),
                pages: Mutex::new(Vec::new()),
                on_page_handlers: Mutex::new(Vec::new()),
                context_routes: Mutex::new(Vec::new()),
                exposed_functions: Mutex::new(Vec::new()),
                utility_page: Mutex::new(None),
                tracing,
                pending_clock_install: Mutex::new(None),
                closed: Mutex::new(false),
                on_close_handlers: Mutex::new(Vec::new()),
                on_console_handlers: Mutex::new(Vec::new()),
                on_request_handlers: Mutex::new(Vec::new()),
                on_response_handlers: Mutex::new(Vec::new()),
                on_requestfailed_handlers: Mutex::new(Vec::new()),
                on_download_handlers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// The CDP browser-context id, or `None` for the default context.
    pub fn browser_context_id(&self) -> Option<&str> {
        self.inner.browser_context_id.as_deref()
    }

    /// Open a new page in this context.
    pub async fn new_page(&self) -> Result<Page> {
        if *self.inner.closed.lock() {
            return Err(Error::TargetClosed {
                target_type: "context".into(),
                context: "context already closed".into(),
            });
        }

        let mut params = json!({"url": "about:blank"});
        if let Some(id) = &self.inner.browser_context_id {
            params["browserContextId"] = json!(id);
        }
        let resp = self
            .inner
            .browser
            .browser_session()
            .send("Target.createTarget", params)
            .await?;
        let target_id = resp
            .get("targetId")
            .and_then(|x| x.as_str())
            .ok_or_else(|| Error::ProtocolError("createTarget missing targetId".into()))?
            .to_string();

        let attach = self
            .inner
            .browser
            .browser_session()
            .send(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
        let session_id = attach
            .get("sessionId")
            .and_then(|x| x.as_str())
            .ok_or_else(|| Error::ProtocolError("attachToTarget missing sessionId".into()))?
            .to_string();

        let init_scripts = self.inner.init_scripts.lock().clone();
        let headers = self.inner.extra_http_headers.lock().clone();
        let ua = self.inner.user_agent.lock().clone();
        let viewport = self.inner.viewport.lock().as_ref().copied();
        let timeout_ms = self.inner.default_timeout_ms.load(Ordering::Relaxed);

        let page = Page::attach(
            self.inner.browser.clone(),
            session_id,
            target_id,
            &init_scripts,
            timeout_ms,
            headers.as_ref(),
            ua.as_deref(),
            viewport,
        )
        .await?;

        self.inner.pages.lock().push(page.clone());

        // Fire on_page handlers.
        let handlers = self.inner.on_page_handlers.lock().clone();
        for h in handlers {
            let p = page.clone();
            tokio::spawn(async move {
                (h)(p).await;
            });
        }

        // Apply context-level routes to the newly created page so handlers
        // registered before the page existed still intercept its requests.
        let routes = self.inner.context_routes.lock().clone();
        for entry in routes {
            let handler = Arc::clone(&entry.handler);
            page.route(&entry.pattern, move |r| {
                let h = Arc::clone(&handler);
                async move {
                    (h)(r).await;
                }
            })
            .await?;
        }

        // Apply stored context defaults/state to the new page.
        page.set_default_navigation_timeout(
            self.inner
                .default_navigation_timeout_ms
                .load(Ordering::Relaxed),
        );
        // If a context-level fake-timer install was registered (via
        // `clock().install()` against a placeholder, or `set_clock_install`),
        // apply it to this fresh page so context-level fake timers carry over.
        if let Some(clock_opts) = self.inner.pending_clock_install.lock().clone() {
            // Best-effort: a clock install failure on a brand-new page is not
            // fatal to page creation — log nothing but continue.
            let _ = Clock::new(page_session_owned(&page))
                .install(Some(clock_opts))
                .await;
        }
        if let Some(geo) = self.inner.geolocation.lock().as_ref().copied() {
            page.set_geolocation(Some((geo.x, geo.y))).await?;
        }
        if *self.inner.offline.lock() {
            page.set_offline(true).await?;
        }

        // Re-expose any context-level functions on the new page.
        let exposed = self.inner.exposed_functions.lock().clone();
        for f in exposed {
            let handler = Arc::clone(&f.handler);
            page.expose_function(&f.name, move |args| {
                let h = Arc::clone(&handler);
                async move { (h)(args).await }
            })
            .await?;
        }

        // Wire context-level event handlers onto this page so each fires for
        // any page in the context (Playwright semantics).
        self.register_page_event_fanout(&page);

        Ok(page)
    }

    /// Register the context's collected event handlers onto one page, so that
    /// a context-level handler fires whenever that page emits the event.
    /// Called for every page as it is created (existing pages registered at
    /// `on_*`-registration time via `apply_event_fanout_to_existing`).
    fn register_page_event_fanout(&self, page: &Page) {
        // on_close
        {
            let handlers = self.inner.on_close_handlers.lock().clone();
            if !handlers.is_empty() {
                page.on_close(move || {
                    let hs = handlers.clone();
                    async move {
                        for h in hs {
                            (h)().await;
                        }
                    }
                });
            }
        }
        // on_console
        {
            let handlers = self.inner.on_console_handlers.lock().clone();
            if !handlers.is_empty() {
                page.on_console(move |msg| {
                    let hs = handlers.clone();
                    async move {
                        for h in hs {
                            (h)(msg.clone()).await;
                        }
                    }
                });
            }
        }
        // on_request
        {
            let handlers = self.inner.on_request_handlers.lock().clone();
            if !handlers.is_empty() {
                page.on_request(move |req| {
                    let hs = handlers.clone();
                    async move {
                        for h in hs {
                            (h)(req.clone()).await;
                        }
                    }
                });
            }
        }
        // on_response
        {
            let handlers = self.inner.on_response_handlers.lock().clone();
            if !handlers.is_empty() {
                page.on_response(move |resp| {
                    let hs = handlers.clone();
                    async move {
                        for h in hs {
                            (h)(resp.clone()).await;
                        }
                    }
                });
            }
        }
        // on_requestfailed
        {
            let handlers = self.inner.on_requestfailed_handlers.lock().clone();
            if !handlers.is_empty() {
                page.on_requestfailed(move |req| {
                    let hs = handlers.clone();
                    async move {
                        for h in hs {
                            (h)(req.clone()).await;
                        }
                    }
                });
            }
        }
        // on_download
        {
            let handlers = self.inner.on_download_handlers.lock().clone();
            if !handlers.is_empty() {
                page.on_download(move |dl| {
                    let hs = handlers.clone();
                    async move {
                        for h in hs {
                            (h)(dl.clone()).await;
                        }
                    }
                });
            }
        }
    }

    /// Add a script evaluated before each document load (applied to new pages).
    pub async fn add_init_script(&self, script: &str) -> Result<()> {
        self.inner.init_scripts.lock().push(script.to_string());
        // Retroactively apply to existing pages.
        let pages = self.inner.pages.lock().clone();
        for p in pages {
            p.add_init_script(script).await?;
        }
        Ok(())
    }

    /// Set the default action timeout (ms) for pages in this context.
    pub fn set_default_timeout(&self, timeout_ms: u64) {
        self.inner.default_timeout_ms.store(timeout_ms, Ordering::Relaxed);
        for p in self.inner.pages.lock().iter() {
            p.set_default_timeout(timeout_ms);
        }
    }

    /// Set extra HTTP headers sent on every request (applied to new + existing pages).
    pub async fn set_extra_http_headers(&self, headers: Headers) -> Result<()> {
        *self.inner.extra_http_headers.lock() = Some(headers.clone());
        let pages = self.inner.pages.lock().clone();
        for p in pages {
            p.set_extra_http_headers(headers.clone()).await?;
        }
        Ok(())
    }

    /// Return a standalone HTTP client ([`APIRequestContext`]) tied to this
    /// context. Its default headers are seeded (best-effort) from the
    /// context's `extra_http_headers`. The client is otherwise independent of
    /// the browser — it makes direct HTTP requests.
    pub fn request(&self) -> APIRequestContext {
        let headers = self
            .inner
            .extra_http_headers
            .lock()
            .clone()
            .unwrap_or_default();
        APIRequestContext::new(headers)
    }

    /// All pages opened in this context.
    pub fn pages(&self) -> Vec<Page> {
        self.inner.pages.lock().clone()
    }

    /// The owning browser.
    pub fn browser(&self) -> Browser {
        self.inner.browser.clone()
    }

    // --- Cookies (via the Storage domain, scoped by browserContextId) --------

    /// Open (once) and cache a hidden about:blank page in this context, for
    /// domain commands that need a page session.
    #[allow(dead_code)]
    async fn ensure_utility_page(&self) -> Result<Page> {
        if let Some(p) = self.inner.utility_page.lock().clone() {
            return Ok(p);
        }
        // Open a hidden about:blank page in this context for domain commands.
        let page = self.new_page().await?;
        *self.inner.utility_page.lock() = Some(page.clone());
        Ok(page)
    }

    /// Add cookies to this context.
    pub async fn add_cookies(&self, cookies: &[crate::types::Cookie]) -> Result<()> {
        // Storage.setCookies with browserContextId is only valid on the
        // browser-level session, so route it there rather than a page session.
        let mut params = json!({ "cookies": serde_json::to_value(cookies)? });
        if let Some(id) = &self.inner.browser_context_id {
            params["browserContextId"] = json!(id);
        }
        self.inner
            .browser
            .browser_session()
            .send("Storage.setCookies", params)
            .await?;
        Ok(())
    }

    /// Get cookies for this context.
    pub async fn cookies(&self) -> Result<Vec<Value>> {
        let mut params = json!({});
        if let Some(id) = &self.inner.browser_context_id {
            params["browserContextId"] = json!(id);
        }
        let v = self
            .inner
            .browser
            .browser_session()
            .send("Storage.getCookies", params)
            .await?;
        let cookies = v
            .get("cookies")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        Ok(cookies
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// Clear cookies for this context.
    pub async fn clear_cookies(&self) -> Result<()> {
        let mut params = json!({});
        if let Some(id) = &self.inner.browser_context_id {
            params["browserContextId"] = json!(id);
        }
        self.inner
            .browser
            .browser_session()
            .send("Storage.clearCookies", params)
            .await?;
        Ok(())
    }

    // --- Events --------------------------------------------------------------

    /// Register a handler called for each new page in this context.
    pub fn on_page<F, Fut>(&self, handler: F)
    where
        F: Fn(Page) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: PageHandler = Arc::new(move |p| Box::pin(handler(p)));
        self.inner.on_page_handlers.lock().push(h);
    }

    /// Close the context and all its pages. Disposes an isolated context.
    pub async fn close(&self) -> Result<()> {
        if *self.inner.closed.lock() {
            return Ok(());
        }
        *self.inner.closed.lock() = true;

        // Close pages.
        let pages = std::mem::take(&mut *self.inner.pages.lock());
        for p in pages {
            let _ = p.close().await;
        }
        if let Some(_util) = self.inner.utility_page.lock().take() {
            // already covered above if tracked; utility may not be in pages list
        }

        if let Some(bcid) = &self.inner.browser_context_id {
            let _ = self
                .inner
                .browser
                .browser_session()
                .send("Target.disposeBrowserContext", json!({"browserContextId": bcid}))
                .await;
        }
        Ok(())
    }

    // ----- Permissions & storage -----

    /// Grant browser permissions (e.g. `["geolocation", "clipboard-read"]`).
    /// Applies browser-wide (CDP `Browser.grantPermissions` is not context-scoped).
    pub async fn grant_permissions(&self, permissions: &[&str]) -> Result<()> {
        self.inner
            .browser
            .browser_session()
            .send(
                "Browser.grantPermissions",
                json!({ "permissions": permissions }),
            )
            .await
            .map(|_: Value| ())
    }

    /// Reset all granted permissions.
    pub async fn clear_permissions(&self) -> Result<()> {
        self.inner
            .browser
            .browser_session()
            .send("Browser.resetPermissions", json!({}))
            .await
            .map(|_: Value| ())
    }

    /// Snapshot storage state: cookies plus per-origin localStorage.
    ///
    /// For each page in the context, `localStorage` is captured grouped by
    /// `location.origin`. Pages whose origin is `"null"` (e.g. `about:blank`,
    /// `data:` URLs) are skipped.
    pub async fn storage_state(&self) -> Result<crate::types::StorageState> {
        let cookies = self.cookies().await?;

        // Collect localStorage per unique origin across all pages.
        let mut origins: Vec<crate::types::OriginStorage> = Vec::new();
        for page in self.pages() {
            // Capture { origin, entries } in one round-trip.
            let v: Value = page
                .evaluate(
                    "JSON.stringify({ origin: location.origin, entries: Object.entries(localStorage).map(([n,v]) => ({name:n, value:v})) })",
                )
                .await?;
            // `evaluate` deserializes the JSON string into a Value (string). Parse it.
            let parsed: Value = match &v {
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                other => other.clone(),
            };
            let origin = parsed
                .get("origin")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string();
            // Skip null-opaque origins (about:blank, data:, etc.).
            if origin.is_empty() || origin == "null" {
                continue;
            }
            let entries = parsed
                .get("entries")
                .and_then(|e| e.as_array())
                .cloned()
                .unwrap_or_default();
            let local_storage: Vec<crate::types::NameValue> = entries
                .into_iter()
                .filter_map(|e| {
                    Some(crate::types::NameValue {
                        name: e.get("name")?.as_str()?.to_string(),
                        value: e.get("value")?.as_str()?.to_string(),
                    })
                })
                .collect();

            if let Some(existing) = origins.iter_mut().find(|o| o.origin == origin) {
                // Merge: later pages win on key collisions.
                for nv in local_storage {
                    if let Some(pos) = existing.localStorage.iter().position(|x| x.name == nv.name) {
                        existing.localStorage[pos].value = nv.value;
                    } else {
                        existing.localStorage.push(nv);
                    }
                }
            } else {
                origins.push(crate::types::OriginStorage {
                    origin,
                    localStorage: local_storage,
                });
            }
        }

        Ok(crate::types::StorageState { cookies, origins })
    }

    /// Restore storage state previously captured by [`Self::storage_state`]:
    /// cookies are added via the Storage domain, and each origin's localStorage
    /// is set by evaluating `localStorage.setItem` on a page at that origin
    /// (an existing page if one matches, otherwise a freshly created one).
    pub async fn set_storage_state(
        &self,
        state: &crate::types::StorageState,
    ) -> Result<()> {
        // --- Cookies ---
        if !state.cookies.is_empty() {
            // The stored cookies are the raw Storage cookie objects; re-apply them
            // through the Storage domain scoped to this context (browser-level
            // session, since browserContextId is only valid there).
            let mut params = json!({ "cookies": state.cookies });
            if let Some(id) = &self.inner.browser_context_id {
                params["browserContextId"] = json!(id);
            }
            self.inner
                .browser
                .browser_session()
                .send("Storage.setCookies", params)
                .await?;
        }

        // --- localStorage per origin ---
        for origin_state in &state.origins {
            if origin_state.localStorage.is_empty() {
                continue;
            }
            // Find an existing page already at this origin, else create one.
            let mut target: Option<Page> = None;
            for p in self.pages() {
                let p_origin: String = match p.evaluate::<String>("location.origin").await {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                if p_origin == origin_state.origin {
                    target = Some(p);
                    break;
                }
            }
            let page = match target {
                Some(p) => p,
                None => {
                    let p = self.new_page().await?;
                    let url = format!("{}/", origin_state.origin);
                    p.goto(&url, None).await?;
                    p
                }
            };

            // Batch all setItem calls in a single eval with the entries as the arg.
            let _: Value = page
                .evaluate_with_arg(
                    "(() => { for (const {name, value} of arg) localStorage.setItem(name, value); return null; })()",
                    &origin_state.localStorage,
                )
                .await?;
        }

        Ok(())
    }

    // ----- Network interception (context-level routing) -----

    /// Register a route that intercepts matching requests on ALL pages in this
    /// context — including pages created after this call. The handler must
    /// continue/fulfill/abort the [`Route`](crate::Route).
    ///
    /// A context route is applied to each page by registering the equivalent
    /// page-level route on it. Context-level and page-level routes are
    /// otherwise independent.
    pub async fn route<F, Fut>(&self, pattern: &str, handler: F) -> Result<()>
    where
        F: Fn(Route) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handler: RouteHandler = Arc::new(move |r| Box::pin(handler(r)));
        let entry = RouteEntry {
            pattern: pattern.to_string(),
            handler: Arc::clone(&handler),
        };
        self.inner.context_routes.lock().push(entry);

        // Apply to every existing page, sharing the same handler Arc.
        let pat = pattern.to_string();
        for page in self.pages() {
            let h = Arc::clone(&handler);
            page.route(&pat, move |r| {
                let h = Arc::clone(&h);
                async move {
                    (h)(r).await;
                }
            })
            .await?;
        }
        Ok(())
    }

    /// Remove the first context-level route registered with exactly `pattern`
    /// and remove the matching page-level route on each page.
    pub async fn unroute(&self, pattern: &str) -> Result<()> {
        let mut routes = self.inner.context_routes.lock();
        if let Some(pos) = routes.iter().position(|e| e.pattern == pattern) {
            routes.remove(pos);
        }
        drop(routes);

        for page in self.pages() {
            page.unroute(pattern).await?;
        }
        Ok(())
    }

    /// Remove all context-level routes and all page-level routes on each page.
    pub async fn unroute_all(&self) -> Result<()> {
        self.inner.context_routes.lock().clear();
        for page in self.pages() {
            page.unroute_all().await?;
        }
        Ok(())
    }

    // ----- HAR replay (route_from_har) --------------------------------------

    /// Replay a HAR file: register one route per HAR entry that serves the
    /// recorded response for matching requests on every page (existing and
    /// future). Mirrors Playwright's `context.route_from_har`.
    ///
    /// Behavior:
    /// - Each HAR entry becomes a context-level [`route`](Self::route) with the
    ///   entry's recorded URL as the pattern. On a match (HTTP method + URL),
    ///   the handler serves the canned response via
    ///   [`Route::fulfill`](crate::Route::fulfill) and returns; non-matching
    ///   requests fall through to the network (the handler calls
    ///   [`Route::fallback`](crate::Route::fallback)).
    /// - If `options.url` is set, only entries whose recorded URL matches that
    ///   glob are installed; the rest are skipped (their requests go to network).
    /// - If `options.update` is `Some(true)`, the file is *not* replayed:
    ///   nothing is fulfilled and all requests fall through to the network.
    ///   (Full HAR recording is deferred — see the module docs of
    ///   [`crate::har`].)
    pub async fn route_from_har(
        &self,
        har_path: impl AsRef<std::path::Path>,
        options: Option<RouteFromHarOptions>,
    ) -> Result<()> {
        let opts = options;
        let parsed = routes_from_har(har_path.as_ref(), opts.as_ref())?;

        // In update mode we deliberately do not fulfill from the HAR; let every
        // request hit the network. Recording itself is deferred.
        let update = opts.as_ref().and_then(|o| o.update).unwrap_or(false);

        for HarRoute {
            method,
            pattern,
            status,
            headers,
            body,
        } in parsed
        {
            let method = method;
            let status = status;
            let headers = headers.clone();
            let body = body.clone();
            let update = update;
            self.route(&pattern, move |route: Route| {
                let method = method.clone();
                let status = status;
                let headers = headers.clone();
                let body = body.clone();
                async move {
                    if update {
                        // Update mode: never fulfill; let the request proceed.
                        let _ = route.fallback().await;
                        return;
                    }
                    // Match the HTTP method (case-insensitive). A mismatched
                    // method means this entry does not own the request — let it
                    // fall through to the next route / the network.
                    if !route.request().method().eq_ignore_ascii_case(&method) {
                        let _ = route.fallback().await;
                        return;
                    }
                    let opts = RouteFulfillOptions {
                        status: Some(status),
                        headers: Some(headers),
                        content_type: None,
                        body: Some(body),
                    };
                    // If fulfill fails for any reason, attempt to continue the
                    // request rather than leaving it stalled.
                    if route.fulfill(opts).await.is_err() {
                        let _ = route.fallback().await;
                    }
                }
            })
            .await?;
        }
        Ok(())
    }

    /// A performance tracing handle bound to the browser-level CDP session.
    ///
    /// Mirrors Playwright's `context.tracing()`. The Tracing domain is
    /// browser-level; `tracing()` returns clones of a single shared handle so
    /// [`Tracing::start`] and [`Tracing::stop`] operate on the same state.
    pub fn tracing(&self) -> crate::Tracing {
        self.inner.tracing.clone()
    }

    // ----- Fake timers (context clock) --------------------------------------
    //
    // Playwright's `context.clock()` controls fake timers for *all* pages in the
    // context. CDP has no fake-timer primitive, so fake timers are injected
    // per-page (see [`crate::clock`]); there is no single session that drives
    // every page. The approach here is:
    //
    //   1. `clock()` returns a [`Clock`] bound to the *first* existing page's
    //      session — so calls like `clock().advance(ms)` / `clock().tick(ms)`
    //      drive that page's virtual clock.
    //   2. To make context-level fake timers apply to pages created *after* the
    //      install, store a pending install config in `ContextInner` and replay
    //      it in `new_page()` via `page.clock().install(...)`.
    //
    // Limitation: `clock()` drives only the first page at the moment of the
    // call; it does not fan out `advance`/`tick` to every existing page. The
    // install (initial seeding) does apply forward to new pages. See report.

    /// Return a fake-timer handle for this context, bound to the first existing
    /// page's session. If no page exists yet, returns a [`Clock`] bound to a
    /// brand-new utility page's session (created lazily); the install options
    /// are *not* applied here — call [`Clock::install`] explicitly or use
    /// [`Self::set_clock_install`] to seed future pages.
    ///
    /// Because fake timers are page-scoped, calls on the returned [`Clock`] only
    /// affect the page it is bound to (the first page). To install fake timers
    /// across all future pages, use [`Self::set_clock_install`].
    pub async fn clock(&self) -> Result<Clock> {
        let pages = self.pages();
        let session = if let Some(first) = pages.into_iter().next() {
            page_session_owned(&first)
        } else {
            // No page yet: open a (cached) utility page and bind to it so the
            // returned Clock is usable. New pages created later still need
            // `set_clock_install` to inherit the fakes.
            let page = self.ensure_utility_page().await?;
            page_session_owned(&page)
        };
        Ok(Clock::new(session))
    }

    /// Register fake-timer install options to be applied to every page created
    /// after this call (and the first existing page immediately). This is the
    /// forward-application mechanism for context-level fake timers.
    ///
    /// Returns the result of installing on the first existing page (if any);
    /// future pages pick up the config in `new_page()`.
    pub async fn set_clock_install(
        &self,
        options: Option<ClockInstallOptions>,
    ) -> Result<()> {
        let opts = options.clone();
        *self.inner.pending_clock_install.lock() = opts;
        // Apply to the first existing page immediately so the caller does not
        // have to create a page first to get the install to take effect.
        if let Some(first) = self.pages().into_iter().next() {
            Clock::new(page_session_owned(&first))
                .install(options.clone())
                .await?;
        }
        Ok(())
    }

    // ----- Context-level event handlers (fan out to every page) -------------
    //
    // Each handler fires when *any* page in the context emits the event. The
    // handler list is stored here and mirrored onto each page at creation and
    // at registration time. Events Page does not expose (`on_weberror`,
    // `on_serviceworker`) are not yet wired — see the report notes.

    /// Register a handler invoked when any page in the context closes.
    pub fn on_close<F, Fut>(&self, handler: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: CloseHandler = Arc::new(move || Box::pin(handler()));
        self.inner.on_close_handlers.lock().push(h.clone());
        for page in self.pages() {
            let h = h.clone();
            page.on_close(move || {
                let hh = h.clone();
                async move { (hh)().await }
            });
        }
    }

    /// Register a handler invoked for every console message on any page.
    pub fn on_console<F, Fut>(&self, handler: F)
    where
        F: Fn(ConsoleMessage) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: ConsoleHandler = Arc::new(move |m| Box::pin(handler(m)));
        self.inner.on_console_handlers.lock().push(h.clone());
        for page in self.pages() {
            let h = h.clone();
            page.on_console(move |m| {
                let hh = h.clone();
                async move { (hh)(m).await }
            });
        }
    }

    /// Register a handler invoked for every request sent by any page.
    pub fn on_request<F, Fut>(&self, handler: F)
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: RequestHandler = Arc::new(move |r| Box::pin(handler(r)));
        self.inner.on_request_handlers.lock().push(h.clone());
        for page in self.pages() {
            let h = h.clone();
            page.on_request(move |r| {
                let hh = h.clone();
                async move { (hh)(r).await }
            });
        }
    }

    /// Register a handler invoked for every response received on any page.
    pub fn on_response<F, Fut>(&self, handler: F)
    where
        F: Fn(Response) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: ResponseHandler = Arc::new(move |r| Box::pin(handler(r)));
        self.inner.on_response_handlers.lock().push(h.clone());
        for page in self.pages() {
            let h = h.clone();
            page.on_response(move |r| {
                let hh = h.clone();
                async move { (hh)(r).await }
            });
        }
    }

    /// Register a handler invoked when any request from any page fails.
    pub fn on_requestfailed<F, Fut>(&self, handler: F)
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: RequestHandler = Arc::new(move |r| Box::pin(handler(r)));
        self.inner.on_requestfailed_handlers.lock().push(h.clone());
        for page in self.pages() {
            let h = h.clone();
            page.on_requestfailed(move |r| {
                let hh = h.clone();
                async move { (hh)(r).await }
            });
        }
    }

    /// Register a handler invoked for each file download on any page.
    pub fn on_download<F, Fut>(&self, handler: F)
    where
        F: Fn(Download) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let h: DownloadHandler = Arc::new(move |d| Box::pin(handler(d)));
        self.inner.on_download_handlers.lock().push(h.clone());
        for page in self.pages() {
            let h = h.clone();
            page.on_download(move |d| {
                let hh = h.clone();
                async move { (hh)(d).await }
            });
        }
    }

    // ----- Emulation / state setters (existing + future pages) --------------

    /// Override geolocation for all current and future pages, or clear it with
    /// `None`.
    ///
    /// **Mapping note:** [`Position`] is the crate's 2D point type
    /// (`{ x, y }`). For geolocation its components are interpreted as
    /// `{ x => latitude, y => longitude }` when forwarded to
    /// [`Page::set_geolocation`].
    pub async fn set_geolocation(&self, geolocation: Option<Position>) -> Result<()> {
        *self.inner.geolocation.lock() = geolocation;
        let geo = geolocation.map(|p| (p.x, p.y));
        for page in self.pages() {
            page.set_geolocation(geo).await?;
        }
        Ok(())
    }

    /// Toggle network-offline emulation for all current and future pages.
    pub async fn set_offline(&self, offline: bool) -> Result<()> {
        *self.inner.offline.lock() = offline;
        for page in self.pages() {
            page.set_offline(offline).await?;
        }
        Ok(())
    }

    /// Set the default navigation timeout (ms) for all current and future
    /// pages.
    pub fn set_default_navigation_timeout(&self, ms: u64) {
        self.inner
            .default_navigation_timeout_ms
            .store(ms, Ordering::Relaxed);
        for page in self.inner.pages.lock().iter() {
            page.set_default_navigation_timeout(ms);
        }
    }

    /// Expose a Rust function to every page in the context as
    /// `window[name]`, including pages created after this call.
    ///
    /// The callback signature matches [`Page::expose_function`]: it receives
    /// the JS arguments as a `Vec<serde_json::Value>` and returns a
    /// `serde_json::Value`.
    pub async fn expose_function<F, Fut>(&self, name: &str, callback: F) -> Result<()>
    where
        F: Fn(Vec<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Value> + Send + 'static,
    {
        let handler: ExposedBindingHandler = Arc::new(move |args| Box::pin(callback(args)));
        self.inner.exposed_functions.lock().push(ExposedFunction {
            name: name.to_string(),
            handler: Arc::clone(&handler),
        });
        let name = name.to_string();
        for page in self.pages() {
            let h = Arc::clone(&handler);
            page.expose_function(&name, move |args| {
                let hh = Arc::clone(&h);
                async move { (hh)(args).await }
            })
            .await?;
        }
        Ok(())
    }

    /// Attach a fresh CDP session to the given page's target and return it.
    ///
    /// Sends `Target.attachToTarget` with `{ targetId, flatten: true }` over
    /// the browser-level session, then wraps the returned `sessionId` in a
    /// target-scoped [`CdpSession`] bound to the same connection.
    pub async fn new_cdp_session(&self, page: &Page) -> Result<CdpSession> {
        let browser_session = self.inner.browser.browser_session();
        let conn = Arc::clone(browser_session.connection());
        let resp: Value = browser_session
            .send(
                "Target.attachToTarget",
                json!({ "targetId": page.target_id(), "flatten": true }),
            )
            .await?;
        let session_id = resp
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::ProtocolError("attachToTarget missing sessionId".into())
            })?
            .to_string();
        Ok(CdpSession::target(conn, session_id))
    }
}
