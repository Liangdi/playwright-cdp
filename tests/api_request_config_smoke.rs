//! Smoke tests for `APIRequestContext` configuration options
//! (`base_url`, `user_agent`, `timeout`, `ignore_https_errors`,
//! `extra_http_headers`).
//!
//! These construct the context directly via `new_with_options`, so they do
//! **not** require a browser. They still guard on `common::browser()` so the
//! suite stays consistent and skips gracefully when the test environment is
//! otherwise headless.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use playwright_cdp::api_request::ApiRequestContextOptions;
use playwright_cdp::APIRequestContext;

#[tokio::test]
async fn base_url_joins_relative_url() {
    let _ = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };

    let base = common::http_server("<api>base-url</api>").await;
    // Strip the trailing '/' so we exercise the join logic: `get("/")` should
    // resolve to `<base>` (which serves at `/`).
    let base = base.trim_end_matches('/').to_string();

    let ctx = APIRequestContext::new_with_options(
        ApiRequestContextOptions::default().base_url(base.clone()),
    );

    // Relative path "/"; base_url should be prepended.
    let resp = ctx.get("/", None).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.ok());
    let body = resp.text().await.unwrap();
    assert!(body.contains("base-url"), "body was: {body}");

    // The final URL (after resolution) should start with the base.
    assert!(
        resp.url().starts_with("http://127.0.0.1:"),
        "url was: {}",
        resp.url()
    );
}

#[tokio::test]
async fn base_url_ignored_for_absolute_url() {
    let _ = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };

    let base = common::http_server("<api>absolute</api>").await;
    let ctx = APIRequestContext::new_with_options(
        ApiRequestContextOptions::default()
            .base_url("http://0.0.0.0:1/nope"),
    );

    // Absolute URL wins over base_url.
    let resp = ctx.get(&base, None).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("absolute"), "body was: {body}");
}

#[tokio::test]
async fn user_agent_and_timeout_do_not_panic() {
    let _ = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };

    let url = common::http_server("<api>ua</api>").await;
    let ctx = APIRequestContext::new_with_options(
        ApiRequestContextOptions::default()
            .user_agent("playwright-cdp-test/1.0")
            .timeout(Duration::from_secs(10))
            .ignore_https_errors(true),
    );

    let resp = ctx.get(&url, None).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.ok());
    let body = resp.text().await.unwrap();
    assert!(body.contains("ua"), "body was: {body}");
}

#[tokio::test]
async fn extra_http_headers_merged() {
    let _ = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };

    let url = common::http_server("<api>headers</api>").await;
    let mut extras: HashMap<String, String> = HashMap::new();
    extras.insert("x-test-header".to_string(), "abc123".to_string());
    let ctx = APIRequestContext::new_with_options(
        ApiRequestContextOptions::default().extra_http_headers(extras),
    );

    // The local server answers 200 regardless of headers; this just verifies
    // the merged-headers path doesn't error and the request still succeeds.
    let resp = ctx.get(&url, None).await.unwrap();
    assert_eq!(resp.status(), 200);

    // default_headers() exposes the seeded extra_http_headers.
    assert_eq!(
        ctx.default_headers().get("x-test-header"),
        Some(&"abc123".to_string())
    );
}
