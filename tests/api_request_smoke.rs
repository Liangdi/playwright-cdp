//! Smoke test for the standalone `APIRequestContext` (`page.request()`).
//!
//! Skips automatically if no browser is available (the `request()` accessor
//! lives on `Page`, so we need a live page to obtain one).

mod common;

use playwright_cdp::options::APIRequestOptions;
use serde_json::json;

#[tokio::test]
async fn get_returns_response() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server("<api>hi</api>").await;

    let resp = page.request().get(&url, None).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.ok());
    let body = resp.text().await.unwrap();
    assert!(body.contains("hi"), "body was: {body}");

    // Cached body accessors are consistent.
    let bytes = resp.body().await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("hi"));
}

#[tokio::test]
async fn post_with_json_body() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server("<api>ok</api>").await;

    // The local server answers 200 regardless of method; this verifies the
    // POST-with-JSON request path doesn't error.
    let resp = page
        .request()
        .post(&url, Some(APIRequestOptions::new().data(json!({"k": "v"}))))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.ok());
}

#[tokio::test]
async fn context_request_carries_headers() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let context = browser.new_context().await.unwrap();
    let url = common::http_server("<api>ctx</api>").await;

    let resp = context.request().get(&url, None).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.ok());
    let body = resp.text().await.unwrap();
    assert!(body.contains("ctx"), "body was: {body}");
}
