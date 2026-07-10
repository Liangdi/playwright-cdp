//! `Mouse` — pointer input via the CDP `Input` domain.

use crate::error::Result;
use crate::options::MouseOptions;
use crate::page::Page;
use crate::types::MouseButton;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Mouse input device. The last-known pointer position is shared across clones
/// created from the same page (mirroring Playwright's persistent `page.mouse()`),
/// so `down()`/`up()` can fire at the position set by the most recent `move_to()`.
#[derive(Clone)]
pub struct Mouse {
    page: Page,
    pos: Arc<Mutex<(f64, f64)>>,
}

impl Mouse {
    /// Wire this mouse to a page-wide shared position cell.
    pub(crate) fn with_pos(page: Page, pos: Arc<Mutex<(f64, f64)>>) -> Self {
        Self { page, pos }
    }

    fn pos(&self) -> (f64, f64) {
        *self.pos.lock()
    }

    async fn dispatch(
        &self,
        r#type: &str,
        x: f64,
        y: f64,
        button: MouseButton,
        buttons: u8,
        click_count: u32,
    ) -> Result<()> {
        self.page
            .session()
            .send(
                "Input.dispatchMouseEvent",
                json!({
                    "type": r#type,
                    "x": x,
                    "y": y,
                    "button": button.as_str(),
                    "buttons": buttons,
                    "clickCount": click_count,
                }),
            )
            .await
            .map(|_: Value| ())
    }

    /// Move the pointer to `(x, y)`.
    pub async fn move_to(&self, x: f64, y: f64, _options: Option<MouseOptions>) -> Result<()> {
        *self.pos.lock() = (x, y);
        self.dispatch("mouseMoved", x, y, MouseButton::Left, 0, 0).await
    }

    /// Press the (currently positioned) button down.
    pub async fn down(&self, options: Option<MouseOptions>) -> Result<()> {
        let opts = options.unwrap_or_default();
        let button = opts.button.unwrap_or_default();
        let (x, y) = self.pos();
        let count = opts.click_count.unwrap_or(1);
        self.dispatch("mousePressed", x, y, button, button.bitmask(), count).await
    }

    /// Release the (currently positioned) button.
    pub async fn up(&self, options: Option<MouseOptions>) -> Result<()> {
        let opts = options.unwrap_or_default();
        let button = opts.button.unwrap_or_default();
        let (x, y) = self.pos();
        let count = opts.click_count.unwrap_or(1);
        self.dispatch("mouseReleased", x, y, button, 0, count).await
    }

    /// Move to `(x, y)` and click once (press + release).
    pub async fn click(&self, x: f64, y: f64, options: Option<MouseOptions>) -> Result<()> {
        self.perform_click(x, y, options, 1).await
    }

    /// Move to `(x, y)` and double-click.
    pub async fn dblclick(&self, x: f64, y: f64, options: Option<MouseOptions>) -> Result<()> {
        self.perform_click(x, y, options, 2).await
    }

    async fn perform_click(
        &self,
        x: f64,
        y: f64,
        options: Option<MouseOptions>,
        default_count: u32,
    ) -> Result<()> {
        let opts = options.unwrap_or_default();
        let button = opts.button.unwrap_or_default();
        let count = opts.click_count.unwrap_or(default_count);
        let delay = opts.delay.map(|ms| Duration::from_millis(ms as u64));

        *self.pos.lock() = (x, y);
        self.dispatch("mouseMoved", x, y, MouseButton::Left, 0, 0).await?;
        for i in 0..count {
            self.dispatch("mousePressed", x, y, button, button.bitmask(), i + 1).await?;
            self.dispatch("mouseReleased", x, y, button, 0, i + 1).await?;
            if let Some(d) = delay {
                if i + 1 < count {
                    tokio::time::sleep(d).await;
                }
            }
        }
        Ok(())
    }

    /// Scroll by `(delta_x, delta_y)` at the current pointer position.
    pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
        let (x, y) = self.pos();
        self.page
            .session()
            .send(
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mouseWheel",
                    "x": x,
                    "y": y,
                    "deltaX": delta_x,
                    "deltaY": delta_y,
                    "button": "none",
                    "buttons": 0,
                }),
            )
            .await
            .map(|_: Value| ())
    }
}
