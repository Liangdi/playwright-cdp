//! `ElementHandle` — a persistent handle to a resolved DOM element (a CDP
//! `RemoteObjectId`).
//!
//! Unlike [`Locator`](crate::Locator), which resolves lazily on each action,
//! an `ElementHandle` pins one specific element. Remote objects are GC'd by the
//! browser when their execution context is destroyed (e.g. on navigation); call
//! [`ElementHandle::dispose`] to release eagerly.

use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::locator::{
    click_element, content_frame_id, drag_mouse, element_box, element_center, hover_element,
    press_key, press_text, select_option_element, set_checked_element,
};
use crate::options::{
    CheckOptions, ClickOptions, DragToOptions, FillOptions, HoverOptions, PressOptions,
    PressSequentiallyOptions, ScreenshotOptions, SelectOption, SelectOptions,
};
use crate::page::Page;
use crate::selectors;
use crate::types::BoundingBox;
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

/// A handle to a resolved element.
#[derive(Clone)]
pub struct ElementHandle {
    page: Page,
    object_id: String,
}

/// An element state that [`ElementHandle::wait_for_element_state`] can poll for.
///
/// Mirrors Playwright's `ElementState` (a subset). `Stable` means the element's
/// bounding box has stopped changing between polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementState {
    /// The element is visible (non-zero box, not `display:none`/`visibility:hidden`).
    Visible,
    /// The element is hidden.
    Hidden,
    /// The element is enabled (not `[disabled]`).
    Enabled,
    /// The element is editable (`contenteditable` or a mutable form field).
    Editable,
    /// The element's bounding box is not changing between successive samples.
    Stable,
}

impl ElementHandle {
    pub(crate) fn new(page: Page, object_id: String) -> Self {
        Self { page, object_id }
    }

    /// The underlying CDP remote object id.
    pub fn object_id(&self) -> &str {
        &self.object_id
    }

    /// The owning page.
    pub fn page(&self) -> Page {
        self.page.clone()
    }

    pub async fn click(&self, options: Option<ClickOptions>) -> Result<()> {
        let opts = options.unwrap_or_default();
        click_element(&self.page, &self.object_id, &opts).await
    }

    pub async fn fill(&self, text: &str, _opts: Option<FillOptions>) -> Result<()> {
        selectors::eval_object(
            self.page.session(),
            &self.object_id,
            "(el, text) => { el.focus(); el.value = text; el.dispatchEvent(new Event('input', { bubbles: true })); el.dispatchEvent(new Event('change', { bubbles: true })); }",
            json!(text),
        )
        .await
        .map(|_| ())
    }

    /// Clear an input/textarea (fill with the empty string).
    pub async fn clear(&self, options: Option<FillOptions>) -> Result<()> {
        self.fill("", options).await
    }

    /// Type each character of `text` sequentially with an optional delay.
    /// Dispatches one `char` key event per character (so per-keystroke handlers
    /// fire). The element is focused first.
    pub async fn type_(&self, text: &str, opts: Option<PressSequentiallyOptions>) -> Result<()> {
        let delay = opts
            .and_then(|o| o.delay)
            .map(|ms| Duration::from_millis(ms as u64));
        let _ = self
            .page
            .session()
            .send("DOM.focus", json!({ "objectId": self.object_id }))
            .await;
        press_text(&self.page, &self.object_id, text, delay).await
    }

    /// Press a single key (focus + keyDown/keyUp).
    pub async fn press(&self, key: &str, options: Option<PressOptions>) -> Result<()> {
        let delay = options
            .and_then(|o| o.delay)
            .map(|ms| Duration::from_millis(ms as u64));
        press_key(&self.page, &self.object_id, key, delay).await
    }

    /// Double-click (clickCount 2).
    pub async fn dblclick(&self, options: Option<ClickOptions>) -> Result<()> {
        let mut opts = options.unwrap_or_default();
        opts.click_count = Some(opts.click_count.unwrap_or(2));
        click_element(&self.page, &self.object_id, &opts).await
    }

    /// Hover over the element.
    pub async fn hover(&self, options: Option<HoverOptions>) -> Result<()> {
        let opts = options.unwrap_or_default();
        hover_element(&self.page, &self.object_id, &opts).await
    }

    /// Tap (touch) the element. Approximated as a click on desktop CDP.
    pub async fn tap(&self, options: Option<ClickOptions>) -> Result<()> {
        self.click(options).await
    }

    /// Check a checkbox/radio if not already checked.
    pub async fn check(&self, options: Option<CheckOptions>) -> Result<()> {
        self.set_checked(true, options).await
    }

    /// Uncheck a checkbox if checked.
    pub async fn uncheck(&self, options: Option<CheckOptions>) -> Result<()> {
        self.set_checked(false, options).await
    }

    /// Set the checked state of a checkbox/radio, clicking only if needed.
    pub async fn set_checked(&self, checked: bool, options: Option<CheckOptions>) -> Result<()> {
        let opts = options.unwrap_or_default();
        set_checked_element(&self.page, &self.object_id, checked, &opts).await
    }

    /// Select an `<option>` by value/label/index. Returns the now-selected values.
    pub async fn select_option(
        &self,
        value: impl Into<SelectOption>,
        _options: Option<SelectOptions>,
    ) -> Result<Vec<String>> {
        let value = value.into();
        let arg: Value = match value {
            SelectOption::Value(v) => json!({ "Value": v }),
            SelectOption::Label(v) => json!({ "Label": v }),
            SelectOption::Index(i) => json!({ "Index": i }),
        };
        select_option_element(&self.page, &self.object_id, arg).await
    }

    /// Drag this element onto `target` via mouse motion.
    pub async fn drag_and_drop(
        &self,
        target: &ElementHandle,
        _options: Option<DragToOptions>,
    ) -> Result<()> {
        let _ = self
            .page
            .session()
            .send("DOM.scrollIntoViewIfNeeded", json!({ "objectId": self.object_id }))
            .await;
        let (sx, sy) = element_center(&self.page, &self.object_id).await?;
        let _ = target
            .page
            .session()
            .send("DOM.scrollIntoViewIfNeeded", json!({ "objectId": target.object_id }))
            .await;
        let (tx, ty) = element_center(&target.page, &target.object_id).await?;
        drag_mouse(&self.page, sx, sy, tx, ty).await
    }

    pub async fn focus(&self) -> Result<()> {
        self.page
            .session()
            .send("DOM.focus", json!({ "objectId": self.object_id }))
            .await
            .map(|_: Value| ())
    }

    pub async fn blur(&self) -> Result<()> {
        selectors::eval_object(self.page.session(), &self.object_id, "(el) => { el.blur(); }", Value::Null)
            .await
            .map(|_| ())
    }

    pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
        self.page
            .session()
            .send("DOM.scrollIntoViewIfNeeded", json!({ "objectId": self.object_id }))
            .await
            .map(|_: Value| ())
    }

    pub async fn dispatch_event(&self, type_: &str, init: Option<Value>) -> Result<()> {
        let init = init.unwrap_or(Value::Null);
        selectors::eval_object(
            self.page.session(),
            &self.object_id,
            "(el, args) => { el.dispatchEvent(new Event(args.type, args.init || undefined)); }",
            json!({ "type": type_, "init": init }),
        )
        .await
        .map(|_| ())
    }

    pub async fn text_content(&self) -> Result<Option<String>> {
        self.read_str_opt("(el) => el.textContent").await
    }

    pub async fn inner_text(&self) -> Result<String> {
        Ok(self.read_str_opt("(el) => el.innerText").await?.unwrap_or_default())
    }

    pub async fn inner_html(&self) -> Result<String> {
        Ok(self.read_str_opt("(el) => el.innerHTML").await?.unwrap_or_default())
    }

    pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
        let v = selectors::eval_object(
            self.page.session(),
            &self.object_id,
            "(el, name) => el.getAttribute(name)",
            json!(name),
        )
        .await?;
        Ok(v.as_str().map(String::from))
    }

    /// Evaluate `expression` with this element bound as the first argument.
    pub async fn evaluate<R: serde::de::DeserializeOwned>(
        &self,
        expression: &str,
        arg: Option<Value>,
    ) -> Result<R> {
        let function = format!("(el, arg) => {{ return ({expression}); }}");
        let v = selectors::eval_object(self.page.session(), &self.object_id, &function, arg.unwrap_or(Value::Null))
            .await?;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    /// Evaluate `page_function` with this element bound as the first argument,
    /// returning a [`JSHandle`](crate::js_handle::JSHandle) to the resulting
    /// remote object (by reference, not by value).
    ///
    /// `page_function` is wrapped as `(el, arg) => { return (<page_function>); }`
    /// and run via `Runtime.callFunctionOn` against this handle's `objectId`
    /// (`returnByValue: false`), mirroring [`ElementHandle::evaluate`].
    pub async fn evaluate_handle(
        &self,
        page_function: &str,
        arg: Option<Value>,
    ) -> Result<crate::js_handle::JSHandle> {
        let function = format!("(el, arg) => {{ return ({page_function}); }}");
        let params = json!({
            "objectId": self.object_id,
            "functionDeclaration": function,
            "arguments": [{ "value": arg.unwrap_or(Value::Null) }],
            "returnByValue": false,
            "awaitPromise": true,
        });
        let resp = self.page.session().send("Runtime.callFunctionOn", params).await?;
        if let Some(exc) = resp.get("exceptionDetails") {
            let msg = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("evaluation threw");
            return Err(Error::ProtocolError(format!("eval error: {msg}")));
        }
        let result_oid = resp
            .get("result")
            .and_then(|r| r.get("objectId"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::ProtocolError(
                    "callFunctionOn did not return a remote object (no objectId)".into(),
                )
            })?
            .to_string();
        Ok(crate::js_handle::JSHandle::new(self.page.session_arc(), result_oid))
    }

    async fn read_str_opt(&self, function: &str) -> Result<Option<String>> {
        let v = selectors::eval_object(self.page.session(), &self.object_id, function, Value::Null).await?;
        Ok(v.as_str().map(String::from))
    }

    async fn state(&self, field: &str) -> Result<bool> {
        let function = format!(
            "(el) => self.__pwcdpInjected && self.__pwcdpInjected.elementState(el).{field}"
        );
        let v = selectors::eval_object(self.page.session(), &self.object_id, &function, Value::Null).await?;
        Ok(v.as_bool().unwrap_or(false))
    }

    pub async fn is_visible(&self) -> Result<bool> {
        self.state("visible").await
    }
    pub async fn is_enabled(&self) -> Result<bool> {
        self.state("enabled").await
    }
    pub async fn is_checked(&self) -> Result<bool> {
        self.state("checked").await
    }
    pub async fn is_editable(&self) -> Result<bool> {
        self.state("editable").await
    }
    pub async fn is_hidden(&self) -> Result<bool> {
        Ok(!self.is_visible().await?)
    }

    pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
        match element_box(&self.page, &self.object_id).await {
            Ok((x, y, w, h)) => Ok(Some(BoundingBox { x, y, width: w, height: h })),
            Err(_) => Ok(None),
        }
    }

    pub async fn screenshot(&self, opts: Option<ScreenshotOptions>) -> Result<Vec<u8>> {
        let (x, y, w, h) = element_box(&self.page, &self.object_id).await?;
        let opts = opts.unwrap_or_default();
        let format = match opts.r#type.unwrap_or_default() {
            crate::types::ScreenshotType::Png => "png",
            crate::types::ScreenshotType::Jpeg => "jpeg",
            crate::types::ScreenshotType::Webp => "webp",
        };
        let mut params = json!({
            "format": format,
            "clip": { "x": x, "y": y, "width": w, "height": h, "scale": 1 },
        });
        if opts.omit_background.unwrap_or(false) && format == "png" {
            params["omitBackground"] = json!(true);
        }
        let resp = self.page.session().send("Page.captureScreenshot", params).await?;
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

    /// Set files on an `<input type=file>` by server-side path(s).
    pub async fn set_input_files(&self, files: &[&str]) -> Result<()> {
        let desc = self
            .page
            .session()
            .send("DOM.requestNode", json!({ "objectId": self.object_id }))
            .await?;
        let node_id = desc
            .get("nodeId")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| Error::ProtocolError("requestNode missing nodeId".into()))?;
        self.page
            .session()
            .send("DOM.setFileInputFiles", json!({ "nodeId": node_id, "files": files }))
            .await
            .map(|_: Value| ())
    }

    /// Release the remote object eagerly (otherwise browser GCs it on navigation).
    pub async fn dispose(&self) -> Result<()> {
        let _ = self
            .page
            .session()
            .send("Runtime.releaseObject", json!({ "objectId": self.object_id }))
            .await;
        Ok(())
    }

    /// If this element is an `<iframe>`, return its content [`Frame`]; else `None`.
    ///
    /// The frame id is read from the DOM node's `contentDocument.frameId`.
    /// Best-effort: returns `None` if the element is not a frame or the content
    /// frame cannot be resolved (e.g. cross-origin, where `contentDocument` is
    /// inaccessible from the embedding context).
    pub async fn content_frame(&self) -> Result<Option<Frame>> {
        let fid = content_frame_id(&self.page, &self.object_id).await?;
        Ok(fid.map(|id| Frame::new(self.page.clone(), id)))
    }

    /// The [`Frame`] that owns this element.
    ///
    /// Resolves the element's owner execution context via the remote object's
    /// frame (DOM.describeNode → frameId on the owning document), falling back
    /// to the page's main frame. Best-effort.
    pub async fn owner_frame(&self) -> Result<Option<Frame>> {
        // The node's owning document carries the frame it lives in.
        let desc = self
            .page
            .session()
            .send("DOM.describeNode", json!({ "objectId": self.object_id }))
            .await?;
        // Walk up to the document node, which carries a frameId.
        let frame_id = desc
            .get("node")
            .and_then(|n| n.get("frameId"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                // contentDocument on an iframe points at the inner frame; for a
                // plain element the owner is the document's frame.
                desc.get("node")
                    .and_then(|n| n.get("owner"))
                    .and_then(|o| o.get("frameId"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| self.page.main_frame_id());
        Ok(frame_id.map(|id| Frame::new(self.page.clone(), id)))
    }

    /// Poll until this element reaches the requested [`ElementState`], using the
    /// page's default timeout.
    ///
    /// `Stable` is detected by sampling the bounding box twice and checking that
    /// it did not move; the other states query the injected element-state helper.
    pub async fn wait_for_element_state(&self, state: ElementState) -> Result<()> {
        let timeout = self.page.default_timeout();
        let deadline = tokio::time::Instant::now() + timeout;
        match state {
            ElementState::Visible | ElementState::Hidden
            | ElementState::Enabled | ElementState::Editable => {
                let field = match state {
                    ElementState::Visible => "visible",
                    ElementState::Hidden => "hidden",
                    ElementState::Enabled => "enabled",
                    ElementState::Editable => "editable",
                    ElementState::Stable => "visible",
                };
                loop {
                    let v = self.state(field).await?;
                    if v {
                        return Ok(());
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(Error::Timeout(format!(
                            "wait_for_element_state({state:?}) timed out after {}ms",
                            timeout.as_millis()
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            ElementState::Stable => {
                // Sample the bounding box twice; stable when it stops moving.
                let mut prev = self.bounding_box().await?;
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let cur = self.bounding_box().await?;
                    let stable = match (prev, cur) {
                        (Some(a), Some(b)) => {
                            (a.x - b.x).abs() < 0.5
                                && (a.y - b.y).abs() < 0.5
                                && (a.width - b.width).abs() < 0.5
                                && (a.height - b.height).abs() < 0.5
                        }
                        (None, None) => true,
                        _ => false,
                    };
                    if stable {
                        return Ok(());
                    }
                    prev = cur;
                    if tokio::time::Instant::now() >= deadline {
                        return Err(Error::Timeout(format!(
                            "wait_for_element_state(Stable) timed out after {}ms",
                            timeout.as_millis()
                        )));
                    }
                }
            }
        }
    }

    /// Always `1` — an `ElementHandle` pins exactly one element. Provided for
    /// API parity with [`Locator::count`](crate::Locator::count); a disposed or
    /// GC'd handle will still report `1` (use [`Self::is_visible`] to check it).
    pub async fn count(&self) -> Result<usize> {
        Ok(1)
    }
}
