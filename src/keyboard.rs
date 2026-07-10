//! `Keyboard` — input via the CDP `Input` domain.

use crate::error::Result;
use crate::options::{PressOptions, PressSequentiallyOptions};
use crate::page::Page;
use serde_json::{json, Value};
use std::time::Duration;

/// Keyboard input device.
#[derive(Clone)]
pub struct Keyboard {
    page: Page,
}

impl Keyboard {
    pub(crate) fn new(page: Page) -> Self {
        Self { page }
    }

    async fn dispatch(
        &self,
        r#type: &str,
        key: Option<&str>,
        code: Option<&str>,
        text: Option<&str>,
    ) -> Result<()> {
        let mut p = json!({ "type": r#type });
        if let Some(k) = key {
            p["key"] = json!(k);
        }
        if let Some(c) = code {
            p["code"] = json!(c);
        }
        if let Some(t) = text {
            p["text"] = json!(t);
        }
        self.page
            .session()
            .send("Input.dispatchKeyEvent", p)
            .await
            .map(|_: Value| ())
    }

    /// Press and hold `key`.
    pub async fn down(&self, key: &str) -> Result<()> {
        self.dispatch("keyDown", Some(key), None, None).await
    }

    /// Release `key`.
    pub async fn up(&self, key: &str) -> Result<()> {
        self.dispatch("keyUp", Some(key), None, None).await
    }

    /// Press `key` (down + up), optionally delaying between the two.
    pub async fn press(&self, key: &str, options: Option<PressOptions>) -> Result<()> {
        let delay = options.and_then(|o| o.delay).map(|ms| Duration::from_millis(ms as u64));
        self.dispatch("keyDown", Some(key), None, None).await?;
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        self.dispatch("keyUp", Some(key), None, None).await
    }

    /// Insert `text` directly, replacing the selection (fast, no per-key delay).
    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.page
            .session()
            .send("Input.insertText", json!({ "text": text }))
            .await
            .map(|_: Value| ())
    }

    /// Type `text`. With no delay this uses [`Self::insert_text`]; with a delay
    /// it types character-by-character (matching Playwright `keyboard.type`).
    pub async fn type_text(&self, text: &str, options: Option<PressSequentiallyOptions>) -> Result<()> {
        let delay = options.and_then(|o| o.delay).map(|ms| Duration::from_millis(ms as u64));
        match delay {
            None => self.insert_text(text).await,
            Some(d) => {
                for ch in text.chars() {
                    let s = ch.to_string();
                    self.dispatch("keyDown", None, None, Some(&s)).await?;
                    tokio::time::sleep(d).await;
                }
                Ok(())
            }
        }
    }

    /// Playwright-style alias for [`Self::type_text`].
    pub async fn type_(&self, text: &str, options: Option<PressSequentiallyOptions>) -> Result<()> {
        self.type_text(text, options).await
    }
}
