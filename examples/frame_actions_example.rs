//! Exercises `Frame` methods DIRECTLY against a child same-origin `<iframe>`
//! (no `frame_locator` workaround):
//! - locate the child frame via `page.frames()`
//! - `child.evaluate(...)` runs in the iframe's own context
//! - `child.get_by_placeholder("Name").fill(...)` + `input_value` assert
//! - `child.add_script_tag({source})` injects an executing `<script>` into the
//!   iframe, verified via `child.evaluate`
//!
//! The iframe is `srcdoc`, so it shares the parent's origin and is reachable
//! through the page's frame tree.
//!
//! Run: `cargo run --example frame_actions_example`

use std::time::Duration;

use playwright_cdp::frame::AddScriptTagOptions;
use playwright_cdp::options::LaunchOptions;
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

    // --- top-level document embedding a same-origin iframe (srcdoc) ---
    page.set_content(
        r#"<html><body><h1>main</h1><iframe id="f" srcdoc="<input id='name' placeholder='Name'><input type='checkbox' id='c'>"></iframe></body></html>"#,
    )
    .await?;

    // --- locate the child frame DIRECTLY from the page's frame tree ---
    let main_id = page.main_frame().frame_id().to_string();
    let child = loop {
        let frames = page.frames().await?;
        let candidate = frames.into_iter().find(|f| {
            f.frame_id() != main_id && !f.is_detached() && f.url().contains("about:srcdoc")
        });
        if let Some(f) = candidate {
            break f;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    println!(
        "child frame attached: id={} url={}",
        child.frame_id(),
        child.url()
    );

    // --- (1) Frame::evaluate runs in the iframe's own context ---
    // Set a value on the iframe's window and read it back through the same
    // child frame. If evaluate were running in the top page, the document
    // title read would be the top page's ("").
    let child_doc_title: String = child.evaluate("document.title").await?;
    println!("child document.title (via Frame::evaluate): {child_doc_title:?}");
    // srcdoc document has no explicit title.
    assert_eq!(child_doc_title, "", "child frame's document.title should be empty");

    // Distinguish the child's window from the top page's: set a marker on the
    // child's window via the child frame, then confirm it is NOT visible from
    // the top page's main context.
    child
        .evaluate::<serde_json::Value>("window.__childMarker = 'from-child-frame'")
        .await?;
    let child_marker: String = child.evaluate("window.__childMarker").await?;
    assert_eq!(child_marker, "from-child-frame", "child.evaluate reads child window");
    let top_marker: Option<String> = page
        .evaluate::<serde_json::Value>("window.__childMarker")
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from));
    assert!(
        top_marker.is_none(),
        "child window marker must NOT leak into the top page's window"
    );
    println!("child.evaluate correctly scoped to the iframe context");

    // --- (2) Frame::get_by_placeholder + fill + input_value ---
    child
        .get_by_placeholder("Name")
        .fill("Alice", None)
        .await?;
    let value = child
        .get_by_placeholder("Name")
        .input_value(None)
        .await?;
    println!("get_by_placeholder input value: {value}");
    assert_eq!(value, "Alice", "placeholder input should be 'Alice'");

    // Cross-check: the top page must NOT see an input#name (it lives in the
    // iframe), proving the locator scoping is child-specific.
    let top_name_count: usize = page
        .evaluate::<serde_json::Value>("document.querySelectorAll('#name').length")
        .await
        .ok()
        .and_then(|v| v.as_u64().map(|n| n as usize))
        .unwrap_or(0);
    assert_eq!(top_name_count, 0, "top document must not contain #name");
    let child_name_count: usize = child
        .evaluate::<serde_json::Value>("document.querySelectorAll('#name').length")
        .await
        .ok()
        .and_then(|v| v.as_u64().map(|n| n as usize))
        .unwrap_or(0);
    assert_eq!(child_name_count, 1, "child document contains #name");

    // --- (3) selector-scoped action via Frame::click inside the iframe ---
    child.locator("#c").click(None).await?;
    let checked: bool = child.locator("#c").evaluate("el.checked", None).await?;
    println!("checkbox checked after click: {checked}");
    assert!(checked, "checkbox should be checked after click");

    // --- (4) Frame::add_script_tag injects an executing <script> ---
    // The injected script sets window.__injected inside the iframe.
    let opts = AddScriptTagOptions::new().source("window.__injected = 42");
    assert_eq!(
        opts.source.as_deref(),
        Some("window.__injected = 42"),
        "AddScriptTagOptions builder round-trips the source"
    );
    child.add_script_tag(Some(opts)).await?;
    // Give the inline script a tick to run.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let injected: i64 = child
        .evaluate::<serde_json::Value>("window.__injected")
        .await
        .ok()
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    println!("injected script set window.__injected = {injected} (in iframe)");
    assert_eq!(
        injected, 42,
        "injected script should set window.__injected = 42 in the iframe"
    );
    // And it must NOT have leaked into the top page.
    let top_injected: Option<i64> = page
        .evaluate::<serde_json::Value>("window.__injected")
        .await
        .ok()
        .and_then(|v| v.as_i64());
    assert!(
        top_injected.is_none(),
        "injected value must NOT leak into the top page"
    );

    println!("PASS");
    browser.close().await?;
    println!("done.");
    Ok(())
}
