//! `Locator` — a Playwright-style element reference resolved lazily against
//! the page's selector engine.

use crate::error::{Error, Result};
use crate::options::{
    CheckOptions, ClickOptions, DragToOptions, FillOptions, FilterOptions, HoverOptions,
    PressOptions, PressSequentiallyOptions, ScreenshotOptions, SelectOption, SelectOptions,
    WaitForOptions,
};
use crate::page::Page;
use crate::selectors;
use crate::types::BoundingBox;
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

/// A lazy element locator.
#[derive(Clone)]
pub struct Locator {
    page: Page,
    selector: String,
    strict: bool,
    forced_index: Option<usize>,
    /// Ordered list of same-origin iframe selectors to descend through before
    /// querying `selector`. Each entry is resolved in the previous frame's
    /// `contentDocument` (the first in the top document). Empty means query
    /// the top document. Populated by [`FrameLocator`](crate::FrameLocator).
    frame_chain: Vec<String>,
}

impl Locator {
    pub(crate) fn new(page: Page, selector: String, strict: bool, forced_index: Option<usize>) -> Self {
        Self {
            page,
            selector,
            strict,
            forced_index,
            frame_chain: Vec::new(),
        }
    }

    /// Construct a locator scoped to a chain of same-origin iframes' content
    /// documents. `frame_chain` is the ordered list of iframe selectors
    /// (outermost first); `selector` is resolved in the deepest frame.
    pub(crate) fn new_in_frame(
        page: Page,
        frame_chain: Vec<String>,
        selector: String,
        strict: bool,
        forced_index: Option<usize>,
    ) -> Self {
        Self {
            page,
            selector,
            strict,
            forced_index,
            frame_chain,
        }
    }

    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// Pick the element at a specific index (disables strict mode).
    pub fn nth(&self, index: i32) -> Locator {
        let mut l = self.clone();
        l.strict = false;
        l.forced_index = Some(if index < 0 { 0 } else { index as usize });
        l
    }

    pub fn first(&self) -> Locator {
        self.nth(0)
    }

    pub fn last(&self) -> Locator {
        let mut l = self.clone();
        l.strict = false;
        l.forced_index = Some(usize::MAX); // resolved at query time
        l
    }

    /// Number of matching elements.
    pub async fn count(&self) -> Result<usize> {
        self.count_query().await
    }

    /// Resolve the count, descending this locator's iframe chain.
    async fn count_query(&self) -> Result<usize> {
        let ctx = self.page.ctx().await;
        if self.frame_chain.is_empty() {
            selectors::count(self.page.session(), ctx, &self.selector).await
        } else {
            selectors::count_in(self.page.session(), ctx, &self.frame_chain, &self.selector).await
        }
    }

    /// Resolve a single matching element to its `RemoteObjectId`, descending
    /// this locator's iframe chain.
    async fn element_query(&self, index: usize) -> Result<Option<String>> {
        let ctx = self.page.ctx().await;
        if self.frame_chain.is_empty() {
            selectors::element_at(self.page.session(), ctx, &self.selector, index).await
        } else {
            selectors::element_at_in(self.page.session(), ctx, &self.frame_chain, &self.selector, index).await
        }
    }

    /// Resolve to a single element RemoteObjectId, enforcing strict mode.
    async fn resolve(&self) -> Result<String> {
        let n = self.count_query().await?;
        if n == 0 {
            return Err(Error::ElementNotFound(format!(
                "no element matched '{}'",
                self.selector
            )));
        }
        if self.strict && self.forced_index.is_none() && n > 1 {
            return Err(Error::ElementNotFound(format!(
                "strict mode violation: {n} elements matched '{}'",
                self.selector
            )));
        }
        let index = match self.forced_index {
            Some(usize::MAX) => n - 1,
            Some(i) => i,
            None => 0,
        };
        self.element_query(index)
            .await?
            .ok_or_else(|| Error::ElementNotFound(format!("no element at index {index} for '{}'", self.selector)))
    }

    async fn release(&self, object_id: &str) {
        let _ = self
            .page
            .session()
            .send("Runtime.releaseObject", json!({ "objectId": object_id }))
            .await;
    }

    // --- actions ---

    pub async fn click(&self, opts: Option<ClickOptions>) -> Result<()> {
        let opts = opts.unwrap_or_default();
        let oid = self.resolve().await?;
        let result = click_element(&self.page, &oid, &opts).await;
        self.release(&oid).await;
        result
    }

    pub async fn fill(&self, text: &str, _opts: Option<FillOptions>) -> Result<()> {
        let oid = self.resolve().await?;
        let result = selectors::eval_object(
            self.page.session(),
            &oid,
            "(el, text) => { el.focus(); el.value = text; el.dispatchEvent(new Event('input', { bubbles: true })); el.dispatchEvent(new Event('change', { bubbles: true })); }",
            json!(text),
        )
        .await
        .map(|_| ());
        self.release(&oid).await;
        result
    }

    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.fill(text, None).await
    }

    /// Playwright-style alias for [`Self::type_text`] (the underscore mirrors
    /// the reserved keyword). `options.delay` is currently ignored (fill-based).
    pub async fn type_(&self, text: &str, _options: Option<PressSequentiallyOptions>) -> Result<()> {
        self.type_text(text).await
    }

    pub async fn press(&self, key: &str, options: Option<PressOptions>) -> Result<()> {
        let oid = self.resolve().await?;
        let _ = self
            .page
            .session()
            .send("DOM.focus", json!({ "objectId": oid }))
            .await;
        let delay = options.and_then(|o| o.delay).map(|ms| Duration::from_millis(ms as u64));
        let down = self
            .page
            .session()
            .send("Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": key }))
            .await;
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        let up = self
            .page
            .session()
            .send("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": key }))
            .await;
        self.release(&oid).await;
        down.map(|_: Value| ())?;
        up.map(|_: Value| ())
    }

    pub async fn hover(&self, opts: Option<HoverOptions>) -> Result<()> {
        let opts = opts.unwrap_or_default();
        let oid = self.resolve().await?;
        let _ = self
            .page
            .session()
            .send("DOM.scrollIntoViewIfNeeded", json!({ "objectId": oid }))
            .await;
        let (bx, by, bw, bh) = element_box(&self.page, &oid).await?;
        let (x, y) = match &opts.position {
            Some(p) => (bx + p.x, by + p.y),
            None => (bx + bw / 2.0, by + bh / 2.0),
        };
        let result = self
            .page
            .session()
            .send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0 }),
            )
            .await
            .map(|_: Value| ());
        self.release(&oid).await;
        result
    }

    pub async fn focus(&self) -> Result<()> {
        let oid = self.resolve().await?;
        let result = self
            .page
            .session()
            .send("DOM.focus", json!({ "objectId": oid }))
            .await
            .map(|_: Value| ());
        self.release(&oid).await;
        result
    }

    /// Remove focus from the element.
    pub async fn blur(&self) -> Result<()> {
        let oid = self.resolve().await?;
        let r = selectors::eval_object(self.page.session(), &oid, "(el) => { el.blur(); }", Value::Null)
            .await
            .map(|_| ());
        self.release(&oid).await;
        r
    }

    /// The current value of an `<input>`/`<textarea>`/`<select>`.
    pub async fn input_value(&self, _options: Option<()>) -> Result<String> {
        let oid = self.resolve().await?;
        let v: Value = {
            let res = selectors::eval_object(
                self.page.session(),
                &oid,
                "(el) => el.value",
                Value::Null,
            )
            .await;
            self.release(&oid).await;
            res?
        };
        Ok(v.as_str().unwrap_or("").to_string())
    }

    /// Scroll the element into view.
    pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
        let oid = self.resolve().await?;
        let r = self
            .page
            .session()
            .send("DOM.scrollIntoViewIfNeeded", json!({ "objectId": oid }))
            .await
            .map(|_: Value| ());
        self.release(&oid).await;
        r
    }

    /// Dispatch a synthetic DOM event on the element.
    pub async fn dispatch_event(&self, type_: &str, init: Option<Value>) -> Result<()> {
        let oid = self.resolve().await?;
        let init = init.unwrap_or(Value::Null);
        let r = selectors::eval_object(
            self.page.session(),
            &oid,
            "(el, args) => { el.dispatchEvent(new Event(args.type, args.init || undefined)); }",
            json!({ "type": type_, "init": init }),
        )
        .await
        .map(|_| ());
        self.release(&oid).await;
        r
    }

    /// Evaluate `expression` with the resolved element bound as the first arg.
    pub async fn evaluate<R: serde::de::DeserializeOwned>(
        &self,
        expression: &str,
        arg: Option<Value>,
    ) -> Result<R> {
        let oid = self.resolve().await?;
        let function = format!("(el, arg) => {{ return ({expression}); }}");
        let v = selectors::eval_object(self.page.session(), &oid, &function, arg.unwrap_or(Value::Null))
            .await?;
        self.release(&oid).await;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    /// Evaluate `expression` against all matching elements (bound as an array).
    pub async fn evaluate_all<R: serde::de::DeserializeOwned>(
        &self,
        expression: &str,
        arg: Option<Value>,
    ) -> Result<R> {
        let function = if self.frame_chain.is_empty() {
            format!(
                "(arg) => {{ const els = self.__pwcdpInjected.querySelectorAll(document, {selector:?}); return ({expression}); }}",
                selector = self.selector,
                expression = expression
            )
        } else {
            // Descend the same-origin iframe chain to the deepest contentDocument.
            format!(
                "(arg) => {{ let root = document; for (const fs of {chain:?}) {{ const f = self.__pwcdpInjected.querySelectorAll(root, fs)[0]; root = f && f.contentDocument; if (!root) {{ const els = []; return ({expression}); }} }} const els = self.__pwcdpInjected.querySelectorAll(root, {selector:?}); return ({expression}); }}",
                chain = self.frame_chain,
                selector = self.selector,
                expression = expression
            )
        };
        let v = selectors::eval_context(
            self.page.session(),
            self.page.ctx().await,
            &function,
            arg.unwrap_or(Value::Null),
        )
        .await?;
        serde_json::from_value::<R>(v).map_err(Error::from)
    }

    // --- info ---

    pub async fn text_content(&self) -> Result<Option<String>> {
        self.read("(el) => el.textContent").await
    }

    pub async fn inner_text(&self) -> Result<String> {
        self.read("(el) => el.innerText")
            .await
            .map(|v: Option<String>| v.unwrap_or_default())
    }

    pub async fn inner_html(&self) -> Result<String> {
        self.read("(el) => el.innerHTML")
            .await
            .map(|v: Option<String>| v.unwrap_or_default())
    }

    pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
        let oid = self.resolve().await?;
        let v = selectors::eval_object(
            self.page.session(),
            &oid,
            "(el, name) => el.getAttribute(name)",
            json!(name),
        )
        .await?;
        self.release(&oid).await;
        Ok(v.as_str().map(String::from))
    }

    async fn read<R: serde::de::DeserializeOwned>(&self, function: &str) -> Result<R> {
        let oid = self.resolve().await?;
        let v = selectors::eval_object(self.page.session(), &oid, function, Value::Null).await?;
        self.release(&oid).await;
        serde_json::from_value::<R>(v).map_err(Error::from)
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

    async fn state(&self, field: &str) -> Result<bool> {
        let oid = self.resolve().await?;
        let function = format!(
            "(el) => self.__pwcdpInjected && self.__pwcdpInjected.elementState(el).{field}"
        );
        let v = selectors::eval_object(self.page.session(), &oid, &function, Value::Null).await?;
        self.release(&oid).await;
        Ok(v.as_bool().unwrap_or(false))
    }

    pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
        let oid = self.resolve().await?;
        let r = element_box(&self.page, &oid).await;
        self.release(&oid).await;
        match r {
            Ok((x, y, w, h)) => Ok(Some(BoundingBox { x, y, width: w, height: h })),
            Err(_) => Ok(None),
        }
    }

    /// Wait until the element resolves (and, for actions, is visible).
    pub async fn wait_for(&self, opts: Option<WaitForOptions>) -> Result<()> {
        let timeout = opts
            .and_then(|o| o.timeout)
            .map(|ms| Duration::from_millis(ms as u64))
            .unwrap_or_else(|| Duration::from_secs(30));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.count().await? > 0 {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "wait_for '{}' timed out after {}ms",
                    self.selector,
                    timeout.as_millis()
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // --- additional actions ---

    /// Clear an input/textarea (fill with empty string).
    pub async fn clear(&self, options: Option<FillOptions>) -> Result<()> {
        self.fill("", options).await
    }

    /// Double-click (two press/release cycles with clickCount 2).
    pub async fn dblclick(&self, options: Option<ClickOptions>) -> Result<()> {
        let mut opts = options.unwrap_or_default();
        opts.click_count = Some(opts.click_count.unwrap_or(2));
        self.click(Some(opts)).await
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
        let current = self.is_checked().await?;
        if current != checked {
            let click_opts = ClickOptions {
                force: opts.force,
                position: opts.position,
                timeout: opts.timeout,
                trial: opts.trial,
                ..Default::default()
            };
            self.click(Some(click_opts)).await?;
        }
        Ok(())
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
        let oid = self.resolve().await?;
        let v: Value = selectors::eval_object(
            self.page.session(),
            &oid,
            "(el, sel) => {
                const opts = Array.from(el.options || []);
                const matched = [];
                for (const o of opts) {
                    const hit = sel.Value != null ? o.value === sel.Value
                            : sel.Label != null ? o.label === sel.Label
                            : sel.Index != null ? o.index === sel.Index : false;
                    if (hit) { o.selected = true; matched.push(o.value); }
                }
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return matched;
            }",
            arg,
        )
        .await?;
        self.release(&oid).await;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default())
    }

    /// Set files on an `<input type=file>` by server-side path(s).
    pub async fn set_input_files(&self, files: &[&str]) -> Result<()> {
        let oid = self.resolve().await?;
        // Resolve the remote object to a DOM nodeId via DOM.describeNode.
        let desc = self
            .page
            .session()
            .send("DOM.requestNode", json!({ "objectId": oid }))
            .await?;
        let node_id = desc
            .get("nodeId")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| Error::ProtocolError("requestNode missing nodeId".into()))?;
        let r = self
            .page
            .session()
            .send(
                "DOM.setFileInputFiles",
                json!({ "nodeId": node_id, "files": files }),
            )
            .await
            .map(|_: Value| ());
        self.release(&oid).await;
        r
    }

    /// Tap (touch) the element. Approximated as a click on desktop CDP.
    pub async fn tap(&self, options: Option<ClickOptions>) -> Result<()> {
        self.click(options).await
    }

    /// Drag this element onto `target` via mouse motion.
    pub async fn drag_to(&self, target: &Locator, _options: Option<DragToOptions>) -> Result<()> {
        let oid = self.resolve().await?;
        let _ = self
            .page
            .session()
            .send("DOM.scrollIntoViewIfNeeded", json!({ "objectId": oid }))
            .await;
        let (sx, sy) = element_center(&self.page, &oid).await?;
        let target_oid = target.resolve().await?;
        let (tx, ty) = element_center(&target.page, &target_oid).await?;
        target.release(&target_oid).await;

        self.page
            .session()
            .send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": sx, "y": sy, "button": "none", "buttons": 0 }),
            )
            .await?;
        self.page
            .session()
            .send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mousePressed", "x": sx, "y": sy, "button": "left", "buttons": 1, "clickCount": 1 }),
            )
            .await?;
        let steps = 10;
        for i in 1..=steps {
            let x = sx + (tx - sx) * (i as f64) / (steps as f64);
            let y = sy + (ty - sy) * (i as f64) / (steps as f64);
            self.page
                .session()
                .send(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 1 }),
                )
                .await?;
        }
        let r = self
            .page
            .session()
            .send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseReleased", "x": tx, "y": ty, "button": "left", "buttons": 0, "clickCount": 1 }),
            )
            .await
            .map(|_: Value| ());
        self.release(&oid).await;
        r
    }

    // --- more info ---

    pub async fn is_hidden(&self) -> Result<bool> {
        Ok(!self.is_visible().await?)
    }

    pub async fn is_disabled(&self) -> Result<bool> {
        let oid = self.resolve().await?;
        let v = selectors::eval_object(
            self.page.session(),
            &oid,
            "(el) => el.disabled === true",
            Value::Null,
        )
        .await?;
        self.release(&oid).await;
        Ok(v.as_bool().unwrap_or(false))
    }

    pub async fn is_focused(&self) -> Result<bool> {
        let oid = self.resolve().await?;
        let v = selectors::eval_object(
            self.page.session(),
            &oid,
            "(el) => el === document.activeElement",
            Value::Null,
        )
        .await?;
        self.release(&oid).await;
        Ok(v.as_bool().unwrap_or(false))
    }

    /// All matching locators (one per resolved element).
    pub async fn all(&self) -> Result<Vec<Locator>> {
        let n = self.count().await?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let mut l = self.clone();
            l.strict = false;
            l.forced_index = Some(i);
            out.push(l);
        }
        Ok(out)
    }

    pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
        let n = self.count().await?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(oid) = self.element_query(i).await? {
                let v = selectors::eval_object(
                    self.page.session(),
                    &oid,
                    "(el) => el.innerText",
                    Value::Null,
                )
                .await
                .ok();
                let _ = self
                    .page
                    .session()
                    .send("Runtime.releaseObject", json!({ "objectId": oid }))
                    .await;
                out.push(v.and_then(|x| x.as_str().map(String::from)).unwrap_or_default());
            }
        }
        Ok(out)
    }

    pub async fn all_text_contents(&self) -> Result<Vec<String>> {
        let n = self.count().await?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(oid) = self.element_query(i).await? {
                let v = selectors::eval_object(
                    self.page.session(),
                    &oid,
                    "(el) => el.textContent",
                    Value::Null,
                )
                .await
                .ok();
                let _ = self
                    .page
                    .session()
                    .send("Runtime.releaseObject", json!({ "objectId": oid }))
                    .await;
                out.push(v.and_then(|x| x.as_str().map(String::from)).unwrap_or_default());
            }
        }
        Ok(out)
    }

    /// Capture a screenshot clipped to this element.
    pub async fn screenshot(&self, opts: Option<ScreenshotOptions>) -> Result<Vec<u8>> {
        let oid = self.resolve().await?;
        let (x, y, w, h) = element_box(&self.page, &oid).await?;
        self.release(&oid).await;

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
        if let Some(q) = opts.quality {
            if format == "jpeg" {
                params["quality"] = json!(q);
            }
        }
        let resp = self
            .page
            .session()
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

    // --- composition ---

    /// Filter matches by text presence (minimal).
    pub fn filter(&self, options: FilterOptions) -> Locator {
        let mut sel = self.selector.clone();
        if let Some(t) = options.has_text {
            sel.push_str(&format!(" >> text={t}"));
        }
        let mut l = self.clone();
        l.selector = sel;
        l
    }

    /// Chain this locator with another (both must match, in order).
    pub fn and_(&self, other: &Locator) -> Locator {
        let mut l = self.clone();
        l.selector = format!("{} >> {}", self.selector, other.selector);
        l
    }

    /// Union with another locator (either matches).
    pub fn or_(&self, other: &Locator) -> Locator {
        let mut l = self.clone();
        l.selector = format!("{}, {}", self.selector, other.selector);
        l
    }

    // --- locator-relative chaining ---

    fn chain(&self, sub: impl Into<String>) -> Locator {
        let mut l = self.clone();
        l.selector = format!("{} >> {}", self.selector, sub.into());
        l.forced_index = None;
        l.strict = true;
        l
    }

    /// Find a descendant matching `selector`.
    pub fn locator(&self, selector: impl Into<String>) -> Locator {
        self.chain(selector)
    }

    pub fn get_by_text(&self, text: &str, _exact: bool) -> Locator {
        self.chain(format!("text={text}"))
    }

    pub fn get_by_label(&self, text: &str) -> Locator {
        self.chain(format!("[aria-label=\"{}\"]", esc(text)))
    }

    pub fn get_by_placeholder(&self, text: &str) -> Locator {
        self.chain(format!("[placeholder=\"{}\"]", esc(text)))
    }

    pub fn get_by_alt_text(&self, text: &str) -> Locator {
        self.chain(format!("[alt=\"{}\"]", esc(text)))
    }

    pub fn get_by_title(&self, text: &str) -> Locator {
        self.chain(format!("[title=\"{}\"]", esc(text)))
    }

    pub fn get_by_test_id(&self, test_id: &str) -> Locator {
        self.chain(format!("[data-testid=\"{}\"]", esc(test_id)))
    }

    pub fn get_by_role(
        &self,
        role: crate::types::AriaRole,
        opts: Option<crate::options::GetByRoleOptions>,
    ) -> Locator {
        let opts = opts.unwrap_or_default();
        let mut sel = format!("role={}", role.as_str());
        if let Some(name) = &opts.name {
            sel.push_str(&format!("[name=\"{name}\"]"));
        }
        if opts.exact == Some(true) {
            sel.push_str("[exact=\"true\"]");
        }
        self.chain(sel)
    }

    // --- aria snapshot ---

    /// Capture an aria-snapshot (Playwright's YAML-ish accessibility-tree
    /// format) rooted at this locator's element.
    ///
    /// Resolves the element to its `RemoteObjectId`, then asks CDP for the
    /// partial accessibility tree rooted there (`fetchRelatives: false`) and
    /// serializes it via [`crate::aria_snapshot`].
    pub async fn aria_snapshot(&self) -> Result<String> {
        let oid = self.resolve().await?;

        // Resolve the element's backend DOM node id, which is how AX nodes link
        // back to the DOM. We then fetch the full AX tree and render the
        // subtree rooted at the matching node. (CDP's `getPartialAXTree` with
        // `fetchRelatives: false` returns only the single node, with no
        // descendants — insufficient for a useful snapshot.)
        let desc = self
            .page
            .session()
            .send("DOM.describeNode", json!({ "objectId": oid }))
            .await;
        self.release(&oid).await;
        let desc = desc?;
        let backend_node_id = desc
            .get("node")
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                Error::ProtocolError("DOM.describeNode missing backendNodeId".into())
            })?;

        // Best-effort: some Chrome builds need the Accessibility domain enabled.
        let _ = self
            .page
            .session()
            .send("Accessibility.enable", json!({}))
            .await;
        let resp = self
            .page
            .session()
            .send("Accessibility.getFullAXTree", json!({}))
            .await?;
        let nodes = resp
            .get("nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(crate::aria_snapshot::serialize(&nodes, Some(backend_node_id)))
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// --- element-level action helpers ---

/// Compute an element's content-box as `(x, y, width, height)` where `(x, y)`
/// is the top-left corner.
pub(crate) async fn element_box(page: &Page, object_id: &str) -> Result<(f64, f64, f64, f64)> {
    let resp = page
        .session()
        .send("DOM.getBoxModel", json!({ "objectId": object_id }))
        .await?;
    let model = resp
        .get("model")
        .ok_or_else(|| Error::ProtocolError("getBoxModel missing model".into()))?;
    let (x, y, w, h) = quad_box(model.get("content"))
        .ok_or_else(|| Error::ProtocolError("getBoxModel content quad malformed".into()))?;
    // Prefer the model's own dimensions when available.
    let mw = model.get("width").and_then(|v| v.as_f64());
    let mh = model.get("height").and_then(|v| v.as_f64());
    Ok((x, y, mw.unwrap_or(w), mh.unwrap_or(h)))
}

/// Center of an element's content box.
pub(crate) async fn element_center(page: &Page, object_id: &str) -> Result<(f64, f64)> {
    let (x, y, w, h) = element_box(page, object_id).await?;
    Ok((x + w / 2.0, y + h / 2.0))
}

/// Wait for an element to be actionable (visible) unless skipping via `force`.
async fn wait_actionable(
    page: &Page,
    object_id: &str,
    timeout: Duration,
) -> Result<()> {
    let function = "(el) => self.__pwcdpInjected && self.__pwcdpInjected.elementState(el).visible";
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let v = selectors::eval_object(page.session(), object_id, function, Value::Null).await?;
        if v.as_bool() == Some(true) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Timeout(format!(
                "element not visible within {}ms",
                timeout.as_millis()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn modifier_key(m: &crate::types::KeyboardModifier) -> &'static str {
    use crate::types::KeyboardModifier::*;
    match m {
        Alt => "Alt",
        Control => "Control",
        ControlOrMeta => "Control",
        Meta => "Meta",
        Shift => "Shift",
    }
}

/// Dispatch a mouse click (press + release) honoring the full `ClickOptions`.
/// Fixes the historical `buttons`-bitmask bug: press uses the button's bitmask,
/// release uses `0`.
pub(crate) async fn click_element(
    page: &Page,
    object_id: &str,
    opts: &ClickOptions,
) -> Result<()> {
    let _ = page
        .session()
        .send("DOM.scrollIntoViewIfNeeded", json!({ "objectId": object_id }))
        .await;

    let force = opts.force.unwrap_or(false);
    let trial = opts.trial.unwrap_or(false);
    if !force {
        let timeout = opts
            .timeout
            .map(|ms| Duration::from_millis(ms as u64))
            .unwrap_or_else(|| page.default_timeout());
        wait_actionable(page, object_id, timeout).await?;
    }

    let (bx, by, bw, bh) = element_box(page, object_id).await?;
    let (x, y) = match &opts.position {
        Some(p) => (bx + p.x, by + p.y),
        None => (bx + bw / 2.0, by + bh / 2.0),
    };
    let button = opts.button.unwrap_or_default();
    let count = opts.click_count.unwrap_or(1);
    let delay = opts.delay.map(|ms| Duration::from_millis(ms as u64));

    // Hold modifiers.
    if let Some(mods) = &opts.modifiers {
        for m in mods {
            let _ = page
                .session()
                .send(
                    "Input.dispatchKeyEvent",
                    json!({ "type": "keyDown", "key": modifier_key(m) }),
                )
                .await;
        }
    }

    let result = if trial {
        Ok(())
    } else {
        let mut out = Ok(());
        page.session()
            .send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0 }),
            )
            .await?;
        for i in 0..count {
            if let Err(e) = page
                .session()
                .send(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mousePressed", "x": x, "y": y, "button": button.as_str(), "buttons": button.bitmask(), "clickCount": i + 1 }),
                )
                .await
            {
                out = Err(e);
                break;
            }
            if let Err(e) = page
                .session()
                .send(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseReleased", "x": x, "y": y, "button": button.as_str(), "buttons": 0, "clickCount": i + 1 }),
                )
                .await
            {
                out = Err(e);
                break;
            }
            if let Some(d) = delay {
                if i + 1 < count {
                    tokio::time::sleep(d).await;
                }
            }
        }
        out
    };

    // Release modifiers.
    if let Some(mods) = &opts.modifiers {
        for m in mods {
            let _ = page
                .session()
                .send(
                    "Input.dispatchKeyEvent",
                    json!({ "type": "keyUp", "key": modifier_key(m) }),
                )
                .await;
        }
    }

    result
}

fn quad_box(quad: Option<&Value>) -> Option<(f64, f64, f64, f64)> {
    let arr = quad.and_then(|v| v.as_array())?;
    let nums: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
    if nums.len() < 8 {
        return None;
    }
    let xs = [nums[0], nums[2], nums[4], nums[6]];
    let ys = [nums[1], nums[3], nums[5], nums[7]];
    let xmin = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let xmax = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let ymin = ys.iter().cloned().fold(f64::INFINITY, f64::min);
    let ymax = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    Some((xmin, ymin, xmax - xmin, ymax - ymin))
}
