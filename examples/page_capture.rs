//! Page capture: full-page + element screenshots, PDF, and aria snapshot.
//!
//! Builds a reasonably-sized page via `set_content` (no network) and captures
//! it in several Playwright-style formats.
//!
//! Run: `cargo run --example page_capture`

use playwright_cdp::options::{LaunchOptions, PdfOptions, ScreenshotOptions};
use playwright_cdp::options::PaperFormat;
use playwright_cdp::types::Viewport;
use playwright_cdp::Playwright;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;
    page.set_viewport_size(Viewport::new(1024, 768)).await?;

    // A page with enough content to make screenshots / PDF meaningful.
    page.set_content(
        r#"
        <!doctype html>
        <html><head><title>Page Capture</title>
          <style>
            body { font-family: sans-serif; margin: 32px; }
            h1 { color: #1a73e8; }
            button { padding: 8px 16px; font-size: 16px; }
          </style>
        </head>
        <body>
          <h1>Page Capture Demo</h1>
          <p>This page exists so screenshots and PDFs have something to render.</p>
          <p>Playwright-cdp drives Chromium directly over CDP; no Node driver.</p>
          <p>A few more paragraphs help the full-page capture span some height,
          and give the accessibility tree a richer shape for the aria snapshot.</p>
          <button id="go">Capture me</button>
          <p>Footer paragraph below the button.</p>
        </body></html>
        "#,
    )
    .await?;

    // Output directory for artifacts (gitignored / harmless).
    tokio::fs::create_dir_all("examples_out").await.ok();

    // --- full-page screenshot ---
    let full = page
        .screenshot(Some(
            ScreenshotOptions::new().full_page(true).path("examples_out/full.png"),
        ))
        .await?;
    println!("full-page screenshot: {} bytes -> examples_out/full.png", full.len());
    assert!(!full.is_empty(), "full-page screenshot should be non-empty");

    // --- element screenshot via a locator ---
    let button = page
        .locator("#go")
        .screenshot(Some(ScreenshotOptions::new().path("examples_out/button.png")))
        .await?;
    println!("element screenshot: {} bytes -> examples_out/button.png", button.len());
    assert!(!button.is_empty(), "element screenshot should be non-empty");

    // --- PDF ---
    let pdf = page.pdf(Some(PdfOptions::new().format(PaperFormat::A4))).await?;
    println!("pdf: {} bytes", pdf.len());
    let head = std::str::from_utf8(&pdf[..pdf.len().min(4)]).unwrap_or("");
    println!("pdf starts with %PDF: {}", head == "%PDF");
    assert!(head == "%PDF", "pdf should start with %PDF");
    assert!(!pdf.is_empty(), "pdf should be non-empty");

    // --- aria snapshot (YAML-ish accessibility tree) ---
    let snapshot = page.aria_snapshot().await?;
    println!("--- aria snapshot ---");
    print!("{snapshot}");
    if !snapshot.ends_with('\n') {
        println!();
    }
    println!("--- end aria snapshot ---");
    assert!(!snapshot.is_empty(), "aria snapshot should be non-empty");

    println!("all captures succeeded.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
