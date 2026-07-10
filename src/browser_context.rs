//! `BrowserContext` — an isolated context (incognito-style), owning pages,
//! init scripts, cookies, and defaults.

use crate::browser::Browser;
use crate::api_request::APIRequestContext;
use crate::error::{Error, Result};
use crate::options::NewContextOptions;
use crate::page::Page;
use crate::route::RouteEntry;
use crate::route::{Route, RouteHandler};
use crate::types::Headers;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

type PageHandler = Arc<
    dyn Fn(Page) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

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
    extra_http_headers: Mutex<Option<Headers>>,
    user_agent: Mutex<Option<String>>,
    viewport: Mutex<Option<crate::types::Viewport>>,
    pages: Mutex<Vec<Page>>,
    on_page_handlers: Mutex<Vec<PageHandler>>,
    /// Context-level route registry. Each entry is applied to every existing
    /// page and to pages created after the route is registered.
    context_routes: Mutex<Vec<RouteEntry>>,
    utility_page: Mutex<Option<Page>>,
    /// A single shared Tracing handle (browser-level session). `tracing()`
    /// returns clones of this so start/stop land on the same state.
    tracing: crate::Tracing,
    closed: Mutex<bool>,
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
                extra_http_headers: Mutex::new(opts.extra_http_headers),
                user_agent: Mutex::new(opts.user_agent),
                viewport: Mutex::new(opts.viewport),
                pages: Mutex::new(Vec::new()),
                on_page_handlers: Mutex::new(Vec::new()),
                context_routes: Mutex::new(Vec::new()),
                utility_page: Mutex::new(None),
                tracing,
                closed: Mutex::new(false),
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

        Ok(page)
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

    /// A performance tracing handle bound to the browser-level CDP session.
    ///
    /// Mirrors Playwright's `context.tracing()`. The Tracing domain is
    /// browser-level; `tracing()` returns clones of a single shared handle so
    /// [`Tracing::start`] and [`Tracing::stop`] operate on the same state.
    pub fn tracing(&self) -> crate::Tracing {
        self.inner.tracing.clone()
    }
}
