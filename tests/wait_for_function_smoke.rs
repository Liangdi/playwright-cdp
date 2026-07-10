//! `Page::wait_for_function` / `Frame::wait_for_function` smoke tests.

mod common;

use playwright_cdp::options::WaitForFunctionOptions;

#[tokio::test]
async fn wait_for_function_returns_once_truthy() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    page.set_content(
        r#"<script>let c=0; setInterval(()=>{c++; window.__c=c}, 20)</script>"#,
    )
    .await
    .unwrap();

    let v: i64 = page
        .wait_for_function(
            "window.__c",
            None,
            Some(WaitForFunctionOptions::new().timeout_ms(2000.0)),
        )
        .await
        .unwrap();
    assert!(v >= 1, "expected window.__c >= 1, got {v}");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn wait_for_function_times_out_on_falsy() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    page.set_content("<html><body></body></html>").await.unwrap();

    let result: Result<i64, _> = page
        .wait_for_function(
            "false",
            None,
            Some(WaitForFunctionOptions::new().timeout_ms(200.0)),
        )
        .await;
    assert!(result.is_err(), "expected wait_for_function(false) to time out");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn frame_wait_for_function_delegates_to_page() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    page.set_content(
        r#"<script>let c=0; setInterval(()=>{c++; window.__c=c}, 20)</script>"#,
    )
    .await
    .unwrap();

    let frame = page.main_frame();
    let v: i64 = frame
        .wait_for_function(
            "window.__c",
            None,
            Some(WaitForFunctionOptions::new().timeout_ms(2000.0)),
        )
        .await
        .unwrap();
    assert!(v >= 1, "expected window.__c >= 1 via frame, got {v}");

    browser.close().await.unwrap();
}
