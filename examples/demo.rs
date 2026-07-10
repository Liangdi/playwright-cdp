//! End-to-end demo: launch Chrome, navigate, evaluate, fill a locator, screenshot.
//!
//! Run: `cargo run --example demo`
//! Requires a Chrome/Chromium on the system, or set `CHROME_PATH`.

use playwright_cdp::options::{GotoOptions, LaunchOptions, WaitUntil};
use playwright_cdp::types::{AriaRole, Viewport};
use playwright_cdp::Playwright;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    // Three-layer entry, mirroring playwright-rust (no Node driver spawned here).
    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;
    page.set_viewport_size(Viewport::new(1280, 720)).await?;

    // --- navigation + evaluation ---
    page.goto("https://example.com", None).await?;
    let title: String = page.evaluate("document.title").await?;
    println!("title after goto: {title}");
    assert!(!title.is_empty(), "title should be non-empty");

    // --- selector engine: get_by_role + click (and verify the navigation happened) ---
    // example.com has a single link ("More information..."); click it without pinning text.
    let link = page.get_by_role(AriaRole::Link, None);
    println!("locator resolved; clicking link...");
    link.click(None).await?;
    // Give the navigation a moment, then read the new url.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let url: String = page.evaluate("location.href").await?;
    println!("navigated to: {url}");
    assert!(
        url.contains("iana.org"),
        "click should have navigated away from example.com, got {url}"
    );

    // --- selector engine: CSS locator + fill ---
    page.goto(
        "data:text/html,<input id=\"q\" placeholder=\"search\"><button>Go</button>",
        None,
    )
    .await?;
    page.locator("#q").fill("hello playwright-cdp", None).await?;
    let value: String = page
        .evaluate("document.getElementById('q').value")
        .await?;
    println!("input value after fill: {value}");
    assert_eq!(value, "hello playwright-cdp");

    let count: usize = page
        .eval_on_selector::<String>("button", "el.tagName")
        .await
        .ok()
        .map(|s| if s == "BUTTON" { 1 } else { 0 })
        .unwrap_or(0);
    println!("button elements found: {count}");

    // --- screenshot ---
    page.goto("https://example.com", Some(GotoOptions::new().wait_until(WaitUntil::Load)))
        .await?;
    let png = page
        .screenshot(Some(playwright_cdp::options::ScreenshotOptions::new().path("demo.png")))
        .await?;
    println!("screenshot bytes: {} -> demo.png", png.len());

    browser.close().await?;
    println!("done.");
    Ok(())
}
