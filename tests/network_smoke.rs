//! Phase D smoke test: goto returns a Response and body/headers are captured.
//!
//! Skips automatically if no browser is available.

mod common;

use playwright_cdp::options::WaitUntil;

#[tokio::test]
async fn goto_returns_response() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server("<html><body><h1>hi</h1></body></html>").await;
    let resp = page.goto(&url, None).await.unwrap().expect("response");
    assert_eq!(resp.status(), 200);
    assert!(resp.ok());
    assert_eq!(resp.url(), url);
    let body = resp.body().await.unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("<h1>hi</h1>"), "body was: {body}");
    assert!(resp.headers().contains_key("content-type"));

    // Request accessor.
    let req = resp.request();
    assert_eq!(req.method(), "GET");
    assert!(req.is_navigation_request());

    browser.close().await.unwrap();
    let _ = WaitUntil::Load; // touch enum
}
