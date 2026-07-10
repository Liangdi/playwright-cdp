//! Smoke test for the generic Playwright-style event waiter `Page::expect_event`.
//!
//! Skips automatically if no browser is available.

mod common;

use std::time::Duration;

use playwright_cdp::Error;

/// `expect_event` captures the `Page.loadEventFired` event fired during a
/// navigation. `tokio::join!` polls the waiter's future first, so its
/// `subscribe()` runs before `goto` triggers the event.
#[tokio::test]
async fn expect_event_captures_load_event() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server("<html><body><h1>hi</h1></body></html>").await;

    let (params, _) = tokio::join!(
        page.expect_event("Page.loadEventFired", Some(Duration::from_secs(5))),
        page.goto(&url, None),
    );

    let params = params.expect("expected the Page.loadEventFired event");
    assert!(params.is_object(), "expected an event params object: {params}");

    browser.close().await.unwrap();
}

/// An event that never fires must time out and return `Error::Timeout`
/// (rather than hanging forever).
#[tokio::test]
async fn expect_event_times_out() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    let res = page
        .expect_event("Something.neverHappens", Some(Duration::from_millis(500)))
        .await;

    match res {
        Ok(params) => panic!("expected a timeout, but got: {params}"),
        Err(Error::Timeout(_)) => {}
        Err(e) => panic!("expected Error::Timeout, got: {e:?}"),
    }

    browser.close().await.unwrap();
}

/// `expect_event` also works for an in-page CDP event (`Runtime.consoleAPICalled`)
/// triggered by evaluating `console.log`.
#[tokio::test]
async fn expect_event_captures_console_api() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    let (params, _) = tokio::join!(
        page.expect_event("Runtime.consoleAPICalled", Some(Duration::from_secs(5))),
        page.evaluate::<serde_json::Value>("console.log('hi')"),
    );

    let params = params.expect("expected the Runtime.consoleAPICalled event");
    // The first console arg is the logged string.
    let first_arg = params
        .get("args")
        .and_then(|a| a.get(0))
        .and_then(|v| v.get("value"))
        .expect("expected a console arg value");
    assert_eq!(first_arg, "hi");

    browser.close().await.unwrap();
}
