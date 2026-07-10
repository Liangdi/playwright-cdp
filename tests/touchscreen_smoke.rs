//! Smoke test for `page.touchscreen().tap(x, y)`. Skips if no browser.

mod common;

use playwright_cdp::options::WaitForFunctionOptions;

#[tokio::test]
async fn touchscreen_tap_fires_touchstart() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(
        r#"<div id="t" style="width:100px;height:100px;background:red"></div>
<script>document.getElementById('t').addEventListener('touchstart', e => { window.__tapped = true; });</script>"#,
    )
    .await
    .unwrap();

    // Element center in viewport coordinates.
    let bb = page.locator("#t").bounding_box().await.unwrap().unwrap();
    let cx = bb.x + bb.width / 2.0;
    let cy = bb.y + bb.height / 2.0;

    page.touchscreen().tap(cx, cy).await.unwrap();

    let tapped: bool = page
        .wait_for_function(
            "window.__tapped",
            None,
            Some(WaitForFunctionOptions::new().timeout_ms(2000.0)),
        )
        .await
        .unwrap();
    assert!(tapped, "touchstart handler should have set window.__tapped");

    browser.close().await.unwrap();
}
