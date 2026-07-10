//! Iframes: the element-scoped `FrameLocator` (same-origin) and the page's
//! frame tree.
//!
//! Builds a page via `set_content` (no network) that embeds a same-origin
//! iframe through `srcdoc`, drives the inner button through a `FrameLocator`,
//! then inspects the page's frame tree.
//!
//! Run: `cargo run --example frames`

use playwright_cdp::options::LaunchOptions;
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
    page.set_viewport_size(Viewport::new(800, 600)).await?;

    // A top-level document with a same-origin iframe (srcdoc shares the
    // parent's origin, so we can reach into its DOM via a FrameLocator).
    page.set_content(
        r#"
        <!doctype html>
        <html><head><title>Frames</title></head>
        <body>
          <h1>outer</h1>
          <p>this content lives in the top-level frame</p>
          <iframe id="f" srcdoc="<button id='b' onclick='window.__clicked=true'>inner button</button><p>inner text</p>"></iframe>
        </body></html>
        "#,
    )
    .await?;

    // Give the iframe a moment to attach before we query into it.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // --- FrameLocator: read text from inside the iframe ---
    let inner_text = page.frame_locator("#f").locator("#b").inner_text().await?;
    println!("inner button text via frame_locator: {inner_text}");
    assert_eq!(inner_text, "inner button");

    // --- FrameLocator: click the inner button and observe its side effect ---
    page.frame_locator("#f").locator("#b").click(None).await?;

    // The onclick ran in the iframe's window, so re-read the flag through the
    // iframe frame's document via a FrameLocator-scoped element evaluate
    // (the matched element is bound as `el`).
    let clicked: bool = page
        .frame_locator("#f")
        .locator("#b")
        .evaluate("el.ownerDocument.defaultView.__clicked === true", None)
        .await?;
    println!("inner button set window.__clicked (in iframe): {clicked}");
    assert!(clicked);

    // --- Page frame tree ---
    let frames = page.frames().await?;
    println!("frame count: {}", frames.len());
    for (i, f) in frames.iter().enumerate() {
        println!("  frame[{i}] name={:?} url={}", f.name(), f.url());
    }
    // Two frames: the top-level and the srcdoc iframe.
    assert_eq!(frames.len(), 2, "expected top-level + iframe");
    assert!(
        frames.iter().any(|f| f.url() == "about:srcdoc"),
        "expected an about:srcdoc frame in the tree"
    );

    let main = page.main_frame();
    println!("main frame id: {}", main.frame_id());
    let children = main.child_frames();
    println!("main frame child count: {}", children.len());
    for (i, c) in children.iter().enumerate() {
        println!("  child[{i}] id={} url={}", c.frame_id(), c.url());
    }
    assert_eq!(children.len(), 1, "main frame should have one child iframe");

    println!("all frame checks succeeded.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
