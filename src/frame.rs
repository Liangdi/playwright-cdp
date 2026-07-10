//! `Frame` — a frame within a page (the main document, or an `<iframe>`).

use crate::element_handle::ElementHandle;
use crate::error::{Error, Result};
use crate::options::{GotoOptions, WaitUntil};
use crate::page::Page;
use crate::response::Response;
use crate::types::AriaRole;
use serde_json::Value;

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
    pub async fn evaluate<R: serde::de::DeserializeOwned>(&self, expression: &str) -> Result<R> {
        self.page.evaluate::<R>(expression).await
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
        options: Option<crate::options::WaitForFunctionOptions>,
    ) -> Result<R> {
        self.page.wait_for_function(expression, arg, options).await
    }

    pub async fn content(&self) -> Result<String> {
        self.page.content().await
    }

    pub async fn set_content(&self, html: &str) -> Result<()> {
        self.page.set_content(html).await
    }

    pub async fn title(&self) -> Result<String> {
        self.page.title().await
    }

    pub fn locator(&self, selector: impl Into<String>) -> crate::locator::Locator {
        crate::locator::Locator::new(self.page.clone(), selector.into(), true, None)
    }

    pub fn get_by_text(&self, text: &str, exact: bool) -> crate::locator::Locator {
        self.page.get_by_text(text, exact)
    }

    pub fn get_by_label(&self, text: &str) -> crate::locator::Locator {
        self.page.get_by_label(text)
    }

    pub fn get_by_role(
        &self,
        role: AriaRole,
        opts: Option<crate::options::GetByRoleOptions>,
    ) -> crate::locator::Locator {
        self.page.get_by_role(role, opts)
    }

    /// The first element matching `selector`, if any.
    pub async fn query_selector(&self, selector: &str) -> Result<Option<ElementHandle>> {
        self.page.query_selector(selector).await
    }

    /// All elements matching `selector`.
    pub async fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementHandle>> {
        self.page.query_selector_all(selector).await
    }

    /// Keep the `Value` import referenced (used by callers wiring args).
    #[allow(dead_code)]
    fn _json_marker(&self) -> Value {
        serde_json::json!({})
    }
}
