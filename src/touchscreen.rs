//! `Touchscreen` — touch input via the CDP `Input.dispatchTouchEvent`.
//!
//! Mirrors Playwright's `page.touchscreen()`. The first tap lazily enables
//! touch emulation once per page (best-effort, errors ignored) so synthetic
//! touch events reach the page's JS event listeners.

use crate::error::Result;
use crate::page::Page;
use crate::types::Position;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Touch input device. Touch emulation is enabled once, lazily, on the first
/// [`Touchscreen::tap`]; the enabled flag is shared across all clones produced
/// from the same page (mirroring Playwright's persistent `page.touchscreen()`).
#[derive(Clone)]
pub struct Touchscreen {
    page: Page,
    touch_emulation_started: Arc<AtomicBool>,
}

impl Touchscreen {
    /// Wire this touchscreen to a page-wide shared emulation-enabled flag.
    pub(crate) fn with_flag(page: Page, flag: Arc<AtomicBool>) -> Self {
        Self {
            page,
            touch_emulation_started: flag,
        }
    }

    /// Enable touch emulation once (best-effort). Idempotent across clones.
    async fn ensure_touch_emulation(&self) {
        if self
            .touch_emulation_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            // setTouchEmulationEnabled is the primary knob; its failure (e.g.
            // on a build without the Emulation domain) is non-fatal — we still
            // attempt to dispatch the touch events themselves.
            let _ = self
                .page
                .session()
                .send(
                    "Emulation.setTouchEmulationEnabled",
                    json!({ "enabled": true }),
                )
                .await;
        }
    }

    /// Dispatch a single tap (touchStart + touchEnd) at viewport coordinates
    /// `(x, y)`, mirroring Playwright's `touchscreen.tap(x, y)`.
    pub async fn tap(&self, x: f64, y: f64) -> Result<()> {
        self.ensure_touch_emulation().await;

        self.page
            .session()
            .send(
                "Input.dispatchTouchEvent",
                json!({
                    "type": "touchStart",
                    "touchPoints": [ { "x": x, "y": y } ],
                    "modifiers": 0,
                }),
            )
            .await
            .map(|_: Value| ())?;

        self.page
            .session()
            .send(
                "Input.dispatchTouchEvent",
                json!({
                    "type": "touchEnd",
                    "touchPoints": [],
                    "modifiers": 0,
                }),
            )
            .await
            .map(|_: Value| ())
    }

    /// Convenience: tap at a [`Position`] (viewport coordinates).
    pub async fn tap_point(&self, p: Position) -> Result<()> {
        self.tap(p.x, p.y).await
    }
}
