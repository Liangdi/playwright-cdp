//! Batch2b smoke: events (pageerror/requestfinished), add_script_tag,
//! networkidle precision.

mod common;

use playwright_cdp::options::{GotoOptions, WaitUntil};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn on_pageerror_captures_uncaught_error() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let got = Arc::new(tokio::sync::Mutex::new(None::<String>));
    let got_clone = got.clone();
    page.on_pageerror(move |msg| {
        let got_clone = got_clone.clone();
        async move {
            *got_clone.lock().await = Some(msg);
        }
    });

    page.set_content(r#"<script>setTimeout(() => { throw new Error("boom") }, 0)</script>"#)
        .await
        .unwrap();
    for _ in 0..50 {
        if got.lock().await.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let msg = got.lock().await.clone().expect("no pageerror captured");
    assert!(msg.contains("boom"), "pageerror was: {msg}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn add_script_tag_runs_inline() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content("<html><body></body></html>").await.unwrap();
    page.add_script_tag(Some("window.__injected = 42;"), None)
        .await
        .unwrap();
    let v: i64 = page.evaluate("window.__injected").await.unwrap();
    assert_eq!(v, 42);

    page.add_style_tag("body { color: red }").await.unwrap();
    let color: String = page
        .evaluate("getComputedStyle(document.body).color")
        .await
        .unwrap();
    assert!(color.contains("255") || color.contains("red"), "color was {color}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn on_requestfinished_and_networkidle() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server("<html><body>ok</body></html>").await;

    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();
    page.on_requestfinished(move |_req| {
        let d = done_clone.clone();
        async move {
            d.store(true, Ordering::SeqCst);
        }
    });

    page.goto(
        &url,
        Some(GotoOptions::new().wait_until(WaitUntil::NetworkIdle)),
    )
    .await
    .unwrap();

    // networkidle returned; give a moment for the finishing event to land.
    for _ in 0..40 {
        if done.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(done.load(Ordering::SeqCst), "no request finished observed");

    browser.close().await.unwrap();
}
