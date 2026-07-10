//! `ElementHandle` — a persistent handle to a resolved DOM element (a CDP
//! `RemoteObjectId`).
//!
//! Unlike [`Locator`](crate::Locator), which resolves lazily on each action,
//! an `ElementHandle` pins one specific element. Remote objects are GC'd by the
//! browser when their execution context is destroyed (e.g. on navigation); call
//! [`ElementHandle::dispose`] to release eagerly.

use crate::error::{Error, Result};
use crate::locator::{click_element, element_box};
use crate::options::{ClickOptions, FillOptions, ScreenshotOptions};
use crate::page::Page;
use crate::selectors;
use crate::types::BoundingBox;
use base64::Engine;
use serde_json::{json, Value};

/// A handle to a resolved element.
#[derive(Clone)]
pub struct ElementHandle {
    page: Page,
    object_id: String,
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
}
