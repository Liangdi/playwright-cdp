//! `Playwright` — the top-level entry point (Playwright-shaped).
//!
//! Unlike upstream Playwright, there is **no Node.js driver** to launch. This
//! handle exists for API-shape parity so callers coming from `playwright-rust`
//! can write the familiar `Playwright::launch().chromium().launch(opts)`. The
//! real engine work lives in [`BrowserType`](crate::BrowserType).

use crate::browser_type::BrowserType;
use crate::error::Result;

/// The Playwright entry point. A lightweight handle — `launch()` performs no
/// process spawning (there is no driver in this crate).
#[derive(Debug, Clone, Copy, Default)]
pub struct Playwright;

impl Playwright {
    /// "Launch" Playwright. No-op beyond returning a handle (no driver here).
    pub async fn launch() -> Result<Self> {
        Ok(Self)
    }

    /// The Chromium browser engine (the only fully-supported engine here).
    pub fn chromium(&self) -> BrowserType {
        BrowserType::chromium()
    }

    /// The Firefox engine. Resolves to Chromium (CDP-only crate); logs a warning on launch.
    pub fn firefox(&self) -> BrowserType {
        BrowserType::new(crate::browser_type::Engine::Firefox)
    }

    /// The WebKit engine. Resolves to Chromium (CDP-only crate); logs a warning on launch.
    pub fn webkit(&self) -> BrowserType {
        BrowserType::new(crate::browser_type::Engine::Webkit)
    }
}
