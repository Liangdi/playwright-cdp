//! Builder option structs mirroring Playwright's option objects.

use crate::types::{
    KeyboardModifier, MouseButton, Position, ProxySettings, ScreenshotClip, ScreenshotType,
    Viewport, DEFAULT_TIMEOUT_MS,
};
use std::collections::HashMap;
use std::time::Duration;

/// Browser launch options. See: <https://playwright.dev/docs/api/class-browsertype#browser-type-launch>
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct LaunchOptions {
    pub headless: Option<bool>,
    pub executable_path: Option<String>,
    pub channel: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,
    pub proxy: Option<ProxySettings>,
    pub slow_mo: Option<f64>,
    pub timeout: Option<f64>,
    pub downloads_path: Option<String>,
    pub traces_dir: Option<String>,
    pub devtools: Option<bool>,
    pub chromium_sandbox: Option<bool>,
    /// Optional persistent user-data-dir. When set, the browser is launched
    /// against this directory instead of a throwaway temp dir, so cookies,
    /// localStorage, etc. persist across runs (Playwright `userDataDir`).
    pub user_data_dir: Option<std::path::PathBuf>,
}

impl LaunchOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn headless(mut self, v: bool) -> Self {
        self.headless = Some(v);
        self
    }
    pub fn executable_path(mut self, v: impl Into<String>) -> Self {
        self.executable_path = Some(v.into());
        self
    }
    pub fn channel(mut self, v: impl Into<String>) -> Self {
        self.channel = Some(v.into());
        self
    }
    pub fn args(mut self, v: Vec<String>) -> Self {
        self.args = Some(v);
        self
    }
    pub fn proxy(mut self, v: ProxySettings) -> Self {
        self.proxy = Some(v);
        self
    }
    pub fn timeout_ms(mut self, v: f64) -> Self {
        self.timeout = Some(v);
        self
    }
    pub fn devtools(mut self, v: bool) -> Self {
        self.devtools = Some(v);
        self
    }
    /// Set a persistent user-data-dir (Playwright `userDataDir`).
    pub fn user_data_dir(mut self, v: impl Into<std::path::PathBuf>) -> Self {
        self.user_data_dir = Some(v.into());
        self
    }

    /// Resolved action/launch timeout in milliseconds.
    pub fn timeout_ms_or_default(&self) -> f64 {
        self.timeout.unwrap_or(DEFAULT_TIMEOUT_MS)
    }
}

/// Options for `connect_over_cdp`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConnectOverCdpOptions {
    pub headers: Option<HashMap<String, String>>,
    pub slow_mo: Option<f64>,
    pub timeout: Option<f64>,
    pub no_defaults: Option<bool>,
}

impl ConnectOverCdpOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn headers(mut self, v: HashMap<String, String>) -> Self {
        self.headers = Some(v);
        self
    }
    pub fn timeout_ms(mut self, v: f64) -> Self {
        self.timeout = Some(v);
        self
    }
}

/// When to consider a navigation finished.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WaitUntil {
    /// Consider navigation complete when the `load` event fires.
    #[default]
    Load,
    /// `DOMContentLoaded`.
    DomContentLoaded,
    /// Heuristic: no in-flight network requests for ~500ms after `load`.
    NetworkIdle,
    /// Resolved as soon as the navigation is committed.
    Commit,
}

impl WaitUntil {
    pub fn as_str(&self) -> &'static str {
        match self {
            WaitUntil::Load => "load",
            WaitUntil::DomContentLoaded => "DOMContentLoaded",
            WaitUntil::NetworkIdle => "networkidle",
            WaitUntil::Commit => "commit",
        }
    }
}

/// Options for navigation methods (`goto`, `reload`, ...).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GotoOptions {
    pub timeout: Option<Duration>,
    pub wait_until: Option<WaitUntil>,
    pub referer: Option<String>,
}

impl GotoOptions {
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
    pub fn referer(mut self, v: impl Into<String>) -> Self {
        self.referer = Some(v.into());
        self
    }
    pub fn wait_until_or_default(&self) -> WaitUntil {
        self.wait_until.unwrap_or_default()
    }
}

/// Options for `Locator::click` / `dblclick`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ClickOptions {
    pub button: Option<MouseButton>,
    pub click_count: Option<u32>,
    pub delay: Option<f64>,
    pub force: Option<bool>,
    pub modifiers: Option<Vec<KeyboardModifier>>,
    pub no_wait_after: Option<bool>,
    pub position: Option<Position>,
    pub timeout: Option<f64>,
    pub trial: Option<bool>,
}

impl ClickOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn timeout_ms(mut self, v: f64) -> Self {
        self.timeout = Some(v);
        self
    }
    pub fn force(mut self, v: bool) -> Self {
        self.force = Some(v);
        self
    }
}

/// Options for `Locator::fill` / `clear`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FillOptions {
    pub force: Option<bool>,
    pub timeout: Option<f64>,
}

impl FillOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn timeout_ms(mut self, v: f64) -> Self {
        self.timeout = Some(v);
        self
    }
}

/// Options for `Locator::press`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PressOptions {
    pub delay: Option<f64>,
    pub timeout: Option<f64>,
}

/// Options for `Keyboard::type_` (sequential key presses).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PressSequentiallyOptions {
    pub delay: Option<f64>,
    pub timeout: Option<f64>,
}

/// Options for `Mouse::click` / `dblclick` / `down` / `up` / `move_to`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MouseOptions {
    pub button: Option<MouseButton>,
    pub click_count: Option<u32>,
    pub delay: Option<f64>,
}

/// Options for `Locator::hover`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct HoverOptions {
    pub force: Option<bool>,
    pub modifiers: Option<Vec<KeyboardModifier>>,
    pub no_wait_after: Option<bool>,
    pub position: Option<Position>,
    pub timeout: Option<f64>,
    pub trial: Option<bool>,
}

/// Options for `Locator::wait_for`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WaitForOptions {
    pub state: Option<WaitForState>,
    pub timeout: Option<f64>,
}

/// Element state to wait for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WaitForState {
    #[default]
    Visible,
    Hidden,
    Attached,
    Detached,
}

/// Options for `Page::screenshot` / `Locator::screenshot`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ScreenshotOptions {
    pub path: Option<String>,
    pub r#type: Option<ScreenshotType>,
    pub quality: Option<u8>,
    pub full_page: Option<bool>,
    pub clip: Option<ScreenshotClip>,
    pub omit_background: Option<bool>,
    pub timeout: Option<f64>,
}

impl ScreenshotOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn path(mut self, v: impl Into<String>) -> Self {
        self.path = Some(v.into());
        self
    }
    pub fn full_page(mut self, v: bool) -> Self {
        self.full_page = Some(v);
        self
    }
    pub fn r#type(mut self, v: ScreenshotType) -> Self {
        self.r#type = Some(v);
        self
    }
    pub fn omit_background(mut self, v: bool) -> Self {
        self.omit_background = Some(v);
        self
    }
}

/// Options for `get_by_role`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GetByRoleOptions {
    pub checked: Option<bool>,
    pub disabled: Option<bool>,
    pub selected: Option<bool>,
    pub expanded: Option<bool>,
    pub include_hidden: Option<bool>,
    pub level: Option<u32>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub exact: Option<bool>,
    pub pressed: Option<bool>,
}

impl GetByRoleOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn name(mut self, v: impl Into<String>) -> Self {
        self.name = Some(v.into());
        self
    }
    pub fn exact(mut self, v: bool) -> Self {
        self.exact = Some(v);
        self
    }
}

/// Whether a new page should be opened with a specific viewport.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct NewContextOptions {
    pub viewport: Option<Viewport>,
    pub user_agent: Option<String>,
    pub locale: Option<String>,
    pub extra_http_headers: Option<HashMap<String, String>>,
    pub ignore_https_errors: Option<bool>,
}

/// A selection criterion for `Locator::select_option`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SelectOption {
    /// Match by the option's `value` attribute.
    Value(String),
    /// Match by the option's visible label text.
    Label(String),
    /// Match by index (0-based).
    Index(u32),
}

impl SelectOption {
    pub fn value(v: impl Into<String>) -> Self {
        Self::Value(v.into())
    }
    pub fn label(v: impl Into<String>) -> Self {
        Self::Label(v.into())
    }
}

/// Options for `Locator::check` / `uncheck` / `set_checked`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CheckOptions {
    pub force: Option<bool>,
    pub position: Option<Position>,
    pub timeout: Option<f64>,
    pub trial: Option<bool>,
}

/// Options for `Locator::select_option`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SelectOptions {
    pub force: Option<bool>,
    pub timeout: Option<f64>,
}

/// Options for `Locator::filter` (minimal).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FilterOptions {
    pub has_text: Option<String>,
    pub has_not_text: Option<String>,
}

/// Emulated media (for `Page::emulate_media`).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct EmulateMediaOptions {
    pub media: Option<Media>,
    pub color_scheme: Option<ColorScheme>,
    pub reduced_motion: Option<ReducedMotion>,
}

/// Emulated `prefers-media` value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Media {
    #[default]
    Screen,
    Print,
}

/// Emulated `prefers-color-scheme` value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
    #[default]
    NoPreference,
}

/// Emulated `prefers-reduced-motion` value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReducedMotion {
    #[default]
    NoPreference,
    Reduce,
}

/// Options for `drag_to` / `drag_and_drop` (minimal).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DragToOptions {
    pub force: Option<bool>,
    pub timeout: Option<f64>,
}

/// Options for `Page::pdf` (minimal subset of CDP `Page.printToPDF`).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PdfOptions {
    pub format: Option<PaperFormat>,
    pub landscape: Option<bool>,
    pub print_background: Option<bool>,
    pub scale: Option<f64>,
    pub margin: Option<PdfMargin>,
    pub prefer_css_page_size: Option<bool>,
}

impl PdfOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn format(mut self, v: PaperFormat) -> Self {
        self.format = Some(v);
        self
    }
    pub fn landscape(mut self, v: bool) -> Self {
        self.landscape = Some(v);
        self
    }
}

/// Standard paper sizes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaperFormat {
    Letter,
    Legal,
    Tabloid,
    Ledger,
    A0,
    A1,
    A2,
    A3,
    A4,
    A5,
    A6,
}

impl PaperFormat {
    /// `(width, height)` in inches.
    pub fn inches(self) -> (f64, f64) {
        match self {
            PaperFormat::Letter => (8.5, 11.0),
            PaperFormat::Legal => (8.5, 14.0),
            PaperFormat::Tabloid => (11.0, 17.0),
            PaperFormat::Ledger => (17.0, 11.0),
            PaperFormat::A0 => (33.1, 46.8),
            PaperFormat::A1 => (23.4, 33.1),
            PaperFormat::A2 => (16.54, 23.4),
            PaperFormat::A3 => (11.7, 16.54),
            PaperFormat::A4 => (8.27, 11.69),
            PaperFormat::A5 => (5.83, 8.27),
            PaperFormat::A6 => (4.13, 5.83),
        }
    }
}

/// PDF page margins in inches.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct PdfMargin {
    pub top: Option<f64>,
    pub bottom: Option<f64>,
    pub left: Option<f64>,
    pub right: Option<f64>,
}

/// Options for [`Tracing::start`](crate::Tracing::start).
///
/// Minimal subset of Playwright's `tracing.start()` options. Captures a Chrome
/// performance trace via the CDP `Tracing` domain (browser-level).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TracingStartOptions {
    /// Trace categories to enable (CDP `Tracing.start` `categories`). If
    /// omitted, Chrome's defaults are used.
    pub categories: Option<Vec<String>>,
    /// Where to write the trace JSON file once [`Tracing::stop`] is called.
    pub path: Option<String>,
    /// When true, captures screenshots (adds the
    /// `disabled-by-default-devtools.screenshot` category).
    pub screenshots: Option<bool>,
}

impl TracingStartOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn categories(mut self, v: Vec<String>) -> Self {
        self.categories = Some(v);
        self
    }
    pub fn path(mut self, v: impl Into<String>) -> Self {
        self.path = Some(v.into());
        self
    }
    pub fn screenshots(mut self, v: bool) -> Self {
        self.screenshots = Some(v);
        self
    }
}

/// Options for `Page::wait_for_function` / `Frame::wait_for_function`.
///
/// Polls `expression` until it yields a truthy value (or `timeout` elapses).
/// See: <https://playwright.dev/docs/api/class-page#page-wait-for-function>
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WaitForFunctionOptions {
    /// Maximum time to wait, in milliseconds. Defaults to the page's default
    /// timeout (30s) when `None`.
    pub timeout: Option<f64>,
    /// Time to wait between evaluations, in milliseconds. Defaults to 100ms.
    pub polling_interval: Option<f64>,
}

impl WaitForFunctionOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// Set the maximum wait time in milliseconds.
    pub fn timeout_ms(mut self, v: f64) -> Self {
        self.timeout = Some(v);
        self
    }
    /// Set the time between polls in milliseconds (default 100).
    pub fn polling_interval_ms(mut self, v: f64) -> Self {
        self.polling_interval = Some(v);
        self
    }
}

/// Options for [`crate::APIRequestContext`] requests.
///
/// Mirrors the relevant subset of Playwright's API request options. `data`
/// (JSON body) and `form` (URL-encoded form) are mutually exclusive; if both
/// are set, `data` wins.
///
/// See: <https://playwright.dev/docs/api/class-apirequestcontext#api-request-context-get>
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct APIRequestOptions {
    /// Per-request headers (merged on top of the context's default headers).
    pub headers: Option<HashMap<String, String>>,
    /// URL query parameters.
    pub params: Option<Vec<(String, String)>>,
    /// JSON request body. Sent as `application/json`.
    pub data: Option<serde_json::Value>,
    /// URL-encoded form body. Sent as `application/x-www-form-urlencoded`.
    pub form: Option<HashMap<String, String>>,
    /// Per-request timeout, in milliseconds.
    pub timeout: Option<f64>,
}

impl APIRequestOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the per-request headers (replaces any previously set headers).
    pub fn headers(mut self, v: HashMap<String, String>) -> Self {
        self.headers = Some(v);
        self
    }

    /// Add a single header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers
            .get_or_insert_with(HashMap::new)
            .insert(name.into(), value.into());
        self
    }

    /// Set the JSON body.
    pub fn data(mut self, v: serde_json::Value) -> Self {
        self.data = Some(v);
        self
    }

    /// Set the URL-encoded form body.
    pub fn form(mut self, v: HashMap<String, String>) -> Self {
        self.form = Some(v);
        self
    }

    /// Set the URL query parameters.
    pub fn params(mut self, v: Vec<(String, String)>) -> Self {
        self.params = Some(v);
        self
    }

    /// Set the per-request timeout in milliseconds.
    pub fn timeout_ms(mut self, v: f64) -> Self {
        self.timeout = Some(v);
        self
    }
}