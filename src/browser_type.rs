//! `BrowserType` — a browser engine entry point (Playwright-shaped).
//!
//! Unlike upstream Playwright, this crate drives Chromium directly over CDP
//! and does **not** spawn a Node.js driver. `BrowserType` is therefore a thin
//! handle whose only real engine is Chromium; `firefox()`/`webkit()` exist for
//! API shape parity and resolve to Chromium (with a warning).

use crate::browser::Browser;
use crate::browser_context::BrowserContext;
use crate::browser_process;
use crate::error::Result;
use crate::options::{ConnectOverCdpOptions, LaunchOptions};
use std::path::PathBuf;

/// Which browser engine a [`BrowserType`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Chromium,
    Firefox,
    Webkit,
}

/// A browser engine. Created from a [`Playwright`](crate::Playwright) handle.
#[derive(Debug, Clone, Copy)]
pub struct BrowserType {
    engine: Engine,
}

impl BrowserType {
    pub(crate) fn new(engine: Engine) -> Self {
        Self { engine }
    }

    /// Chromium engine (the only fully-supported engine here).
    pub fn chromium() -> Self {
        Self::new(Engine::Chromium)
    }

    /// Engine name: `"chromium"`, `"firefox"`, or `"webkit"`.
    pub fn name(&self) -> &'static str {
        match self.engine {
            Engine::Chromium => "chromium",
            Engine::Firefox => "firefox",
            Engine::Webkit => "webkit",
        }
    }

    /// Best-effort discovery of the executable path for this engine.
    pub fn executable_path(&self) -> Option<PathBuf> {
        browser_process::discover_executable(&LaunchOptions::default()).ok()
    }

    /// Launch a browser with default options.
    pub async fn launch(&self) -> Result<Browser> {
        self.launch_with_options(LaunchOptions::default()).await
    }

    /// Launch a browser with the given options.
    ///
    /// Only Chromium is driven here; `firefox`/`webkit` log a warning and fall
    /// back to launching Chromium (kept for porting compatibility).
    pub async fn launch_with_options(&self, opts: LaunchOptions) -> Result<Browser> {
        if !matches!(self.engine, Engine::Chromium) {
            tracing::warn!(
                engine = self.name(),
                "playwright-cdp only drives Chromium via CDP; launching Chromium instead"
            );
        }
        Browser::launch(opts).await
    }

    /// Launch a browser with a persistent user-data-dir and return its default
    /// [`BrowserContext`]. Cookies / localStorage persist to `user_data_dir`
    /// across runs. Mirrors Playwright's `browserType.launchPersistentContext`.
    ///
    /// `user_data_dir` accepts any path-like value (`impl AsRef<Path>`), so you
    /// can pass a `&str`, `&Path`, `&PathBuf`, or an owned `PathBuf` directly.
    pub async fn launch_persistent_context(
        &self,
        user_data_dir: impl AsRef<std::path::Path>,
    ) -> Result<BrowserContext> {
        self.launch_persistent_context_with_options(user_data_dir, LaunchOptions::default())
            .await
    }

    /// Like [`launch_persistent_context`](Self::launch_persistent_context) but with
    /// custom launch options. The `user_data_dir` passed here overrides any
    /// `user_data_dir` set on `opts`.
    ///
    /// `user_data_dir` accepts any path-like value (`impl AsRef<Path>`), so you
    /// can pass a `&str`, `&Path`, `&PathBuf`, or an owned `PathBuf` directly.
    pub async fn launch_persistent_context_with_options(
        &self,
        user_data_dir: impl AsRef<std::path::Path>,
        mut opts: LaunchOptions,
    ) -> Result<BrowserContext> {
        if !matches!(self.engine, Engine::Chromium) {
            tracing::warn!(
                engine = self.name(),
                "playwright-cdp only drives Chromium via CDP; launching Chromium instead"
            );
        }
        opts.user_data_dir = Some(PathBuf::from(user_data_dir.as_ref()));
        let browser = Browser::launch(opts).await?;
        // The persistent context is the browser's default (non-isolated) context:
        // browser_context_id == None, backed by the on-disk user-data-dir.
        let ctx = BrowserContext::default_for(browser);
        // Owning context: close() must tear the whole browser down, matching
        // Playwright's persistent-context semantics.
        ctx.mark_persistent();
        Ok(ctx)
    }

    /// Attach to an already-running browser over CDP.
    pub async fn connect_over_cdp(
        &self,
        endpoint: &str,
        opts: Option<ConnectOverCdpOptions>,
    ) -> Result<Browser> {
        Browser::connect_over_cdp(endpoint, opts).await
    }
}
