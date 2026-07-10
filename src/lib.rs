//! # playwright-cdp
//!
//! Drive Chromium-based browsers **directly** via the Chrome DevTools Protocol
//! (CDP) over a single WebSocket — **no Playwright Node.js driver required**.
//!
//! The public API mirrors Playwright's shape (`Playwright`, `BrowserType`,
//! `Browser`, `BrowserContext`, `Page`, `Locator`, builder option structs,
//! `async + Result<T>`), but it speaks CDP natively instead of proxying
//! through Playwright's driver.
//!
//! ```no_run
//! # use playwright_cdp::{Playwright, BrowserType, options::LaunchOptions};
//! # #[tokio::main]
//! # async fn main() -> playwright_cdp::Result<()> {
//! // Three-layer entry, mirroring playwright-rust (no driver spawned here).
//! let browser = Playwright::launch().await?
//!     .chromium()
//!     .launch_with_options(LaunchOptions::default()).await?;
//! let page = browser.new_page().await?;
//! page.goto("https://example.com", None).await?;
//! let title: String = page.evaluate("document.title").await?;
//! println!("{title}");
//! browser.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! `Browser::launch` also works directly for callers who don't need the
//! `Playwright`/`BrowserType` layer.

pub mod api_request;
pub(crate) mod aria_snapshot;
pub mod assertions;
pub mod browser;
pub mod browser_context;
pub mod browser_process;
pub mod browser_type;
pub mod cdp;
pub mod download;
pub mod element_handle;
pub mod error;
pub mod file_chooser;
pub mod frame;
pub mod frame_locator;
pub mod keyboard;
pub mod locator;
pub mod mouse;
mod network;
pub mod options;
pub mod page;
pub mod playwright;
pub mod request;
pub mod response;
pub mod route;
pub mod selectors;
pub mod tracing;
pub mod touchscreen;
pub mod types;
pub mod worker;

// --- Feature modules (filled by sub-agents; declared here so the crate
// stays compilable while individual modules are authored). ---
pub mod clock;
pub mod coverage;
pub mod video;
pub mod web_storage;
pub mod web_socket;
pub mod js_handle;
pub mod accessibility;
pub mod har;

pub use api_request::{APIRequestContext, APIResponse, ApiRequestContextOptions};
pub use assertions::{expect, expect_page, LocatorAssertions, PageAssertions};
pub use browser::Browser;
pub use browser_context::BrowserContext;
pub use browser_type::{BrowserType, Engine};
pub use cdp::session::CdpSession;
pub use download::Download;
pub use element_handle::ElementHandle;
pub use error::{Error, Result};
pub use file_chooser::FileChooser;
pub use frame::Frame;
pub use frame_locator::FrameLocator;
pub use keyboard::Keyboard;
pub use locator::Locator;
pub use mouse::Mouse;
pub use page::{Dialog, Page};
pub use playwright::Playwright;
pub use request::Request;
pub use response::Response;
pub use options::APIRequestOptions;
pub use route::{Route, RouteContinueOptions, RouteFetchOptions, RouteFetchResponse, RouteFulfillOptions};
pub use types::{AriaRole, ConsoleMessage, MouseButton, NameValue, OriginStorage, Position, StorageState, Viewport};
pub use tracing::Tracing;
pub use touchscreen::Touchscreen;
pub use worker::Worker;

// --- New feature-module re-exports (Playwright-compatible handles). ---
pub use accessibility::{Accessibility, AccessibilityNode, AccessibilitySnapshotOptions};
pub use clock::{Clock, ClockInstallOptions};
pub use coverage::{
    Coverage, CSSCoverageEntry, CSSCoverageResult, JSCoverageEntry, JSCoverageResult,
};
pub use element_handle::ElementState;
pub use har::{Har, HarEntry, HarRecorder, HarRequest, HarResponse, RouteFromHarOptions};
pub use js_handle::JSHandle;
pub use video::{Video, VideoStartOptions};
pub use web_socket::{
    FrameData, FrameDirection, WebSocket, WebSocketFrame, WebSocketHandshake,
    WebSocketLiveEvent,
};
pub use web_storage::WebStorage;
// Network detail DTOs surfaced by `Request` / `Response`.
pub use network::{ServerAddr, SecurityDetails, Sizes, Timing};
