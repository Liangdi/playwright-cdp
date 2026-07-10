//! `Frame` — a frame within a page (the main document, or an `<iframe>`).

use crate::element_handle::ElementHandle;
use crate::error::{Error, Result};
use crate::options::{
    CheckOptions, ClickOptions, DragToOptions, FillOptions, GotoOptions, HoverOptions, PressOptions,
    PressSequentiallyOptions, SelectOption, SelectOptions, WaitForFunctionOptions, WaitForOptions, WaitUntil,
};
use crate::page::Page;
use crate::response::Response;
use crate::types::AriaRole;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::broadcast;

/// Options for [`Frame::add_script_tag`].
///
/// Either `url` (load a remote script) or `source` (inline script text) must
/// be supplied. `type` sets the `<script type=...>` attribute (e.g.
/// `"module"` / `"text/javascript"`).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AddScriptTagOptions {
    /// The `src` URL of an external script to load.
    pub url: Option<String>,
    /// Inline script source text to inject as the element's text content.
    pub source: Option<String>,
    /// The `<script type=...>` attribute value (e.g. `"module"`).
    pub r#type: Option<String>,
}

impl AddScriptTagOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn url(mut self, v: impl Into<String>) -> Self {
        self.url = Some(v.into());
        self
    }
    pub fn source(mut self, v: impl Into<String>) -> Self {
        self.source = Some(v.into());
        self
    }
    pub fn type_(mut self, v: impl Into<String>) -> Self {
        self.r#type = Some(v.into());
        self
    }
}

/// Options for [`Frame::add_style_tag`].
///
/// Either `url` (load a remote stylesheet) or `source` (inline CSS text) must
/// be supplied.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AddStyleTagOptions {
    /// The `href` URL of an external stylesheet to load.
    pub url: Option<String>,
    /// Inline CSS source text to inject as the element's text content.
    pub source: Option<String>,
}

impl AddStyleTagOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn url(mut self, v: impl Into<String>) -> Self {
        self.url = Some(v.into());
        self
    }
    pub fn source(mut self, v: impl Into<String>) -> Self {
        self.source = Some(v.into());
        self
    }
}

/// Options for [`Frame::wait_for_url`].
///
/// Mirrors the subset of Playwright's `FrameWaitForUrlOptions` we support: a
/// navigation-style timeout plus an optional lifecycle state to settle on.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FrameWaitForUrlOptions {
    pub timeout: Option<Duration>,
    pub wait_until: Option<WaitUntil>,
}

impl FrameWaitForUrlOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn timeout(mut self, v: Duration) -> Self {
        self.timeout = Some(v);
        self
    }
    pub fn wait_until(mut self, v: WaitUntil) -> Self {
        self.wait_until = Some(v);
        self
    }
}

/// A frame in a page. The main frame wraps the page's top-level document.
#[derive(Clone)]
pub struct Frame {
    page: Page,
    frame_id: String,
}

impl Frame {
    pub(crate) fn new(page: Page, frame_id: String) -> Self {
        Self { page, frame_id }
    }

    pub(crate) fn main(page: Page) -> Self {
        let frame_id = page.main_frame_id().unwrap_or_default();
        Self { page, frame_id }
    }

    /// The owning page.
    pub fn page(&self) -> Page {
        self.page.clone()
    }

    /// CDP frame id.
    pub fn frame_id(&self) -> &str {
        &self.frame_id
    }

    pub fn name(&self) -> String {
        self.page
            .frame_data(&self.frame_id)
            .map(|d| d.name)
            .unwrap_or_default()
    }

    pub fn url(&self) -> String {
        self.page
            .frame_data(&self.frame_id)
            .map(|d| d.url)
            .unwrap_or_default()
    }

    pub fn parent_frame(&self) -> Option<Frame> {
        let parent = self.page.frame_data(&self.frame_id)?.parent_id.clone()?;
        Some(Frame::new(self.page.clone(), parent))
    }

    pub fn child_frames(&self) -> Vec<Frame> {
        self.page
            .frame_ids_with_parent(&self.frame_id)
            .into_iter()
            .map(|id| Frame::new(self.page.clone(), id))
            .collect()
    }

    pub fn is_detached(&self) -> bool {
        self.page
            .frame_data(&self.frame_id)
            .map(|d| d.detached)
            .unwrap_or(false)
    }

    /// Evaluate `expression` in this frame's main world.
    ///
    /// Runs in the frame's own execution context (looked up from the page's
    /// per-frame context map), so for a child `<iframe>` it acts on the
    /// iframe's document/window, not the top-level page's.
    pub async fn evaluate<R: serde::de::DeserializeOwned>(&self, expression: &str) -> Result<R> {
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let function = format!("(arg) => {{ return ({expression}); }}");
        let v = crate::selectors::eval_context(self.page.session(), ctx, &function, Value::Null)
            .await?;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    /// Evaluate `expression` in this frame's main world, returning a
    /// [`JSHandle`](crate::js_handle::JSHandle) to the resulting remote object.
    ///
    /// Mirrors [`Frame::evaluate`] but returns the object by reference rather
    /// than by value. The expression runs in the frame's own execution context
    /// (`Runtime.evaluate { contextId }`, `returnByValue: false`); the returned
    /// `objectId` is wrapped in a [`JSHandle`](crate::js_handle::JSHandle)
    /// sharing the page's session.
    pub async fn evaluate_handle(&self, expression: &str) -> Result<crate::js_handle::JSHandle> {
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let function = format!("(arg) => {{ return ({expression}); }}");
        let oid = crate::selectors::eval_context_handle(
            self.page.session(),
            ctx,
            &function,
            Value::Null,
        )
        .await?
        .ok_or_else(|| {
            Error::ProtocolError(
                "evaluate_handle did not return a remote object (no objectId)".into(),
            )
        })?;
        Ok(crate::js_handle::JSHandle::new(self.page.session_arc(), oid))
    }

    /// Navigate this frame to `url` (main frame only).
    pub async fn goto(
        &self,
        url: &str,
        opts: Option<GotoOptions>,
    ) -> Result<Option<Response>> {
        if self.frame_id != self.page.main_frame_id().unwrap_or_default() {
            return Err(Error::InvalidArgument(
                "Frame::goto on non-main frames is not supported".into(),
            ));
        }
        self.page.goto(url, opts).await
    }

    pub async fn wait_for_load_state(&self, state: Option<WaitUntil>) -> Result<()> {
        self.page.wait_for_load_state(state).await
    }

    /// Poll `expression` until truthy (delegates to the page). See
    /// [`Page::wait_for_function`].
    pub async fn wait_for_function<R: serde::de::DeserializeOwned>(
        &self,
        expression: &str,
        arg: Option<serde_json::Value>,
        options: Option<WaitForFunctionOptions>,
    ) -> Result<R> {
        self.page.wait_for_function(expression, arg, options).await
    }

    /// The serialized HTML of this frame's document (`documentElement.outerHTML`).
    pub async fn content(&self) -> Result<String> {
        self.evaluate::<String>("document.documentElement.outerHTML").await
    }

    /// Replace this frame's document content with `html`.
    ///
    /// Runs `document.open()/write()/close()` in the frame's own context, so
    /// for a child iframe it rewrites the iframe's document.
    pub async fn set_content(&self, html: &str) -> Result<()> {
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let _ = crate::selectors::eval_context(
            self.page.session(),
            ctx,
            "(html) => { document.open(); document.write(html); document.close(); }",
            json!(html),
        )
        .await?;
        Ok(())
    }

    /// The title of this frame's document (`document.title`).
    pub async fn title(&self) -> Result<String> {
        self.evaluate::<String>("document.title").await
    }

    /// Build a locator scoped to this frame's execution context.
    ///
    /// The resolved locator runs its selector queries in the frame's own
    /// context, so for a child iframe `document` is the iframe's document.
    /// This is the same-origin scoping mechanism used by [`Frame::evaluate`].
    pub fn locator(&self, selector: impl Into<String>) -> crate::locator::Locator {
        let ctx = self.page.context_for_frame(&self.frame_id);
        crate::locator::Locator::new_in_frame_ctx(
            self.page.clone(),
            ctx,
            selector.into(),
            true,
            None,
        )
    }

    pub fn get_by_text(&self, text: &str, exact: bool) -> crate::locator::Locator {
        let sel = if exact {
            format!("text=\"{text}\"")
        } else {
            format!("text={text}")
        };
        self.locator(sel)
    }

    pub fn get_by_label(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[aria-label=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_role(
        &self,
        role: AriaRole,
        opts: Option<crate::options::GetByRoleOptions>,
    ) -> crate::locator::Locator {
        let opts = opts.unwrap_or_default();
        let mut sel = format!("role={}", role.as_str());
        if let Some(name) = &opts.name {
            sel.push_str(&format!("[name=\"{name}\"]"));
        }
        if opts.exact == Some(true) {
            sel.push_str("[exact=\"true\"]");
        }
        self.locator(sel)
    }

    pub fn get_by_placeholder(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[placeholder=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_alt_text(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[alt=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_title(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[title=\"{}\"]", attr_escape(text)))
    }

    pub fn get_by_test_id(&self, text: &str) -> crate::locator::Locator {
        self.locator(format!("[data-testid=\"{}\"]", attr_escape(text)))
    }

    /// Inject a `<script>` tag into this frame.
    ///
    /// Either `url` (external script) or `source` (inline script text) must be
    /// provided; `type` sets the `<script type=...>` attribute.
    ///
    /// Implemented via DOM injection in the frame's own execution context, so
    /// `document.head` is this frame's `<head>` — for a child iframe the script
    /// is injected into (and executes within) the iframe's document.
    pub async fn add_script_tag(&self, opts: Option<AddScriptTagOptions>) -> Result<()> {
        let opts = opts.unwrap_or_default();
        if opts.url.is_none() && opts.source.is_none() {
            return Err(Error::InvalidArgument(
                "AddScriptTagOptions requires either `url` or `source`".into(),
            ));
        }
        let arg = json!({ "url": opts.url, "source": opts.source, "type": opts.r#type });
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let _ = crate::selectors::eval_context(
            self.page.session(),
            ctx,
            "(a) => { const s = document.createElement('script'); if (a.type) s.type = a.type; if (a.url) s.src = a.url; if (a.source) s.textContent = a.source; document.head.appendChild(s); }",
            arg,
        )
        .await?;
        Ok(())
    }

    /// Inject a `<link rel=stylesheet>`/`<style>` tag into this frame.
    ///
    /// Either `url` (external stylesheet) or `source` (inline CSS) must be
    /// provided. Runs in the frame's own context, so it targets this frame's
    /// `<head>`.
    pub async fn add_style_tag(&self, opts: Option<AddStyleTagOptions>) -> Result<()> {
        let opts = opts.unwrap_or_default();
        if opts.url.is_none() && opts.source.is_none() {
            return Err(Error::InvalidArgument(
                "AddStyleTagOptions requires either `url` or `source`".into(),
            ));
        }
        let arg = json!({ "url": opts.url, "source": opts.source });
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let _ = crate::selectors::eval_context(
            self.page.session(),
            ctx,
            "(a) => { if (a.url) { const l = document.createElement('link'); l.rel = 'stylesheet'; l.href = a.url; document.head.appendChild(l); } else { const s = document.createElement('style'); s.textContent = a.source; document.head.appendChild(s); } }",
            arg,
        )
        .await?;
        Ok(())
    }

    /// Wait until this frame's URL matches `url` (exact, or `*` glob).
    ///
    /// Subscribes to the page session and watches `Page.frameNavigated` /
    /// `Page.lifecycleEvent` events for this frame, falling back to the cached
    /// frame tree URL. On match, optionally waits for a lifecycle state.
    pub async fn wait_for_url(&self, url: &str, opts: Option<FrameWaitForUrlOptions>) -> Result<()> {
        let opts = opts.unwrap_or_default();
        let timeout = opts
            .timeout
            .unwrap_or_else(|| self.page.default_navigation_timeout());
        let deadline = tokio::time::Instant::now() + timeout;

        // Fast path: already matches.
        if glob_matches(url, &self.url()) {
            if let Some(state) = opts.wait_until {
                self.wait_for_load_state(Some(state)).await?;
            }
            return Ok(());
        }

        // Subscribe before polling so navigation/lifecycle events for this
        // frame drive the loop (the background frame tracker refreshes the
        // cached URL from `Page.frameNavigated`).
        let mut rx = self.page.session().subscribe();
        loop {
            // Re-check the cached URL (kept fresh by the frame tracker task).
            if glob_matches(url, &self.url()) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "wait_for_url '{url}' timed out after {}ms (last: '{}')",
                    timeout.as_millis(),
                    self.url()
                )));
            }
            // Block until the next CDP event, bounded by the remaining budget.
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(ev))
                    if frame_event_matches(&ev, &self.frame_id) =>
                {
                    continue;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(Error::ChannelClosed);
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Err(_) => {
                    return Err(Error::Timeout(format!(
                        "wait_for_url '{url}' timed out after {}ms (last: '{}')",
                        timeout.as_millis(),
                        self.url()
                    )));
                }
            }
        }
        if let Some(state) = opts.wait_until {
            self.wait_for_load_state(Some(state)).await?;
        }
        Ok(())
    }

    // --- Frame action methods (thin delegations to locators) ---
    //
    // Each resolves a locator for `selector` and forwards the call, mirroring
    // Playwright's `frame.*` API shape (selector first, options last).

    /// Click the first element matching `selector`.
    pub async fn click(&self, selector: &str, opts: Option<ClickOptions>) -> Result<()> {
        self.locator(selector).click(opts).await
    }

    /// Double-click the first element matching `selector`.
    pub async fn dblclick(&self, selector: &str, opts: Option<ClickOptions>) -> Result<()> {
        self.locator(selector).dblclick(opts).await
    }

    /// Set the value of the first matching `<input>`/`<textarea>` to `value`.
    pub async fn fill(&self, selector: &str, value: &str, opts: Option<FillOptions>) -> Result<()> {
        self.locator(selector).fill(value, opts).await
    }

    /// Focus the first element matching `selector`.
    pub async fn focus(&self, selector: &str) -> Result<()> {
        self.locator(selector).focus().await
    }

    /// Hover the first element matching `selector`.
    pub async fn hover(&self, selector: &str, opts: Option<HoverOptions>) -> Result<()> {
        self.locator(selector).hover(opts).await
    }

    /// Press `key` on the first element matching `selector`.
    pub async fn press(&self, selector: &str, key: &str, opts: Option<PressOptions>) -> Result<()> {
        self.locator(selector).press(key, opts).await
    }

    /// Select `<option>`(s) on the first matching `<select>` by value/label/index.
    pub async fn select_option(
        &self,
        selector: &str,
        value: impl Into<SelectOption>,
        opts: Option<SelectOptions>,
    ) -> Result<Vec<String>> {
        self.locator(selector).select_option(value, opts).await
    }

    /// Set files on the first matching `<input type=file>` by server-side path(s).
    pub async fn set_input_files(&self, selector: &str, files: &[&str]) -> Result<()> {
        self.locator(selector).set_input_files(files).await
    }

    /// Tap (touch) the first element matching `selector`.
    pub async fn tap(&self, selector: &str, opts: Option<ClickOptions>) -> Result<()> {
        self.locator(selector).tap(opts).await
    }

    /// Check the first matching checkbox/radio.
    pub async fn check(&self, selector: &str, opts: Option<CheckOptions>) -> Result<()> {
        self.locator(selector).check(opts).await
    }

    /// Uncheck the first matching checkbox.
    pub async fn uncheck(&self, selector: &str, opts: Option<CheckOptions>) -> Result<()> {
        self.locator(selector).uncheck(opts).await
    }

    /// Set the checked state of the first matching checkbox/radio.
    pub async fn set_checked(
        &self,
        selector: &str,
        checked: bool,
        opts: Option<CheckOptions>,
    ) -> Result<()> {
        self.locator(selector)
            .set_checked(checked, opts)
            .await
    }

    /// Type `text` into the first element matching `selector` (fill-based).
    pub async fn type_(
        &self,
        selector: &str,
        text: &str,
        opts: Option<PressSequentiallyOptions>,
    ) -> Result<()> {
        self.locator(selector).type_(text, opts).await
    }

    /// The `textContent` of the first element matching `selector`, if any.
    pub async fn text_content(&self, selector: &str) -> Result<Option<String>> {
        self.locator(selector).text_content().await
    }

    /// The visible text of the first element matching `selector`.
    pub async fn inner_text(&self, selector: &str) -> Result<String> {
        self.locator(selector).inner_text().await
    }

    /// The serialized HTML of the first element matching `selector`.
    pub async fn inner_html(&self, selector: &str) -> Result<String> {
        self.locator(selector).inner_html().await
    }

    /// The value of attribute `name` on the first element matching `selector`.
    pub async fn get_attribute(&self, selector: &str, name: &str) -> Result<Option<String>> {
        self.locator(selector).get_attribute(name).await
    }

    /// Number of elements matching `selector` in this frame.
    pub async fn count(&self, selector: &str) -> Result<usize> {
        self.locator(selector).count().await
    }

    /// Wait until an element matching `selector` resolves in this frame.
    pub async fn wait_for_selector(
        &self,
        selector: &str,
        opts: Option<WaitForOptions>,
    ) -> Result<()> {
        self.locator(selector).wait_for(opts).await
    }

    /// Evaluate `expression` against the first element matching `selector` in
    /// this frame.
    pub async fn eval_on_selector<R: serde::de::DeserializeOwned>(
        &self,
        selector: &str,
        expression: &str,
    ) -> Result<R> {
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let object_id = crate::selectors::element_at(self.page.session(), ctx, selector, 0)
            .await?
            .ok_or_else(|| Error::ElementNotFound(selector.to_string()))?;
        let function = format!("(el) => {{ return ({expression}); }}");
        let v = crate::selectors::eval_object(self.page.session(), &object_id, &function, Value::Null)
            .await?;
        self.release_object(&object_id).await;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    /// Evaluate `expression` against all elements matching `selector` in this
    /// frame.
    pub async fn eval_on_selector_all<R: serde::de::DeserializeOwned>(
        &self,
        selector: &str,
        expression: &str,
    ) -> Result<Vec<R>> {
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let n = crate::selectors::count(self.page.session(), ctx, selector).await?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(oid) =
                crate::selectors::element_at(self.page.session(), ctx, selector, i).await?
            {
                let function = format!("(el) => {{ return ({expression}); }}");
                let v =
                    crate::selectors::eval_object(self.page.session(), &oid, &function, Value::Null)
                        .await?;
                self.release_object(&oid).await;
                out.push(serde_json::from_value::<R>(v).map_err(Error::from)?);
            }
        }
        Ok(out)
    }

    /// Dispatch a synthetic DOM event on the first element matching `selector`.
    pub async fn dispatch_event(
        &self,
        selector: &str,
        type_: &str,
        init: Option<Value>,
    ) -> Result<()> {
        self.locator(selector)
            .dispatch_event(type_, init)
            .await
    }

    /// Drag the element matching `source` onto the element matching `target`.
    pub async fn drag_and_drop(
        &self,
        source: &str,
        target: &str,
        opts: Option<DragToOptions>,
    ) -> Result<()> {
        self.locator(source).drag_to(&self.locator(target), opts).await
    }

    /// The first element matching `selector` in this frame, if any.
    pub async fn query_selector(&self, selector: &str) -> Result<Option<ElementHandle>> {
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let oid = crate::selectors::element_at(self.page.session(), ctx, selector, 0)
            .await?
            .map(|oid| ElementHandle::new(self.page.clone(), oid));
        Ok(oid)
    }

    /// All elements matching `selector` in this frame.
    pub async fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementHandle>> {
        let ctx = self.page.ctx_for_frame(&self.frame_id).await;
        let n = crate::selectors::count(self.page.session(), ctx, selector).await?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(oid) =
                crate::selectors::element_at(self.page.session(), ctx, selector, i).await?
            {
                out.push(ElementHandle::new(self.page.clone(), oid));
            }
        }
        Ok(out)
    }

    async fn release_object(&self, object_id: &str) {
        let _ = self
            .page
            .session()
            .send("Runtime.releaseObject", json!({ "objectId": object_id }))
            .await;
    }

    /// Keep the `Value` import referenced (used by callers wiring args).
    #[allow(dead_code)]
    fn _json_marker(&self) -> Value {
        serde_json::json!({})
    }
}

/// Escape a string for safe embedding in a CSS-attribute selector.
fn attr_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Whether a CDP event is one that can change this frame's URL/state — i.e. a
/// `Page.frameNavigated` or `Page.lifecycleEvent` whose `frameId`/`frame.id`
/// matches `frame_id`.
fn frame_event_matches(ev: &crate::cdp::CdpEvent, frame_id: &str) -> bool {
    match ev.method.as_str() {
        "Page.frameNavigated" => ev
            .params
            .get("frame")
            .and_then(|f| f.get("id"))
            .and_then(|v| v.as_str())
            == Some(frame_id),
        "Page.lifecycleEvent" => {
            ev.params.get("frameId").and_then(|v| v.as_str()) == Some(frame_id)
        }
        _ => false,
    }
}

/// Match `pattern` against `value`: exact, or a leading/trailing/both `*` glob.
///
/// Mirrors the helper in `src/page.rs` used by `Page::wait_for_url`.
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
