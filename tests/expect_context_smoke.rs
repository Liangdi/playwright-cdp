//! Smoke test for the `BrowserContext`-level event-wait assertions
//! `expect_page` / `expect_request` / `expect_console`.
//!
//! Skips automatically if no browser is available.

mod common;

use std::time::Duration;

/// `expect_page` resolves with a new page opened in the context. `ctx.new_page`
/// itself fires `on_page`, so racing the wait against `new_page` exercises the
/// one-shot handler. `tokio::join!` polls the `expect_page` future first, which
/// registers its handler synchronously before the first await point — so the
/// handler is in place before `new_page` runs far enough to fire it.
#[tokio::test]
async fn expect_page_resolves_on_new_page() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let ctx = browser.new_context().await.unwrap();

    let (page, _) = tokio::join!(
        ctx.expect_page(Some(Duration::from_secs(5))),
        ctx.new_page(),
    );

    let page = page.expect("expected a page from expect_page");
    // The page handle is usable; querying its url confirms it is live.
    let _ = page.url().await;

    ctx.close().await.unwrap();
    browser.close().await.unwrap();
}

/// `expect_request` resolves with the request whose URL contains the
/// predicate, fired from a page in this context.
#[tokio::test]
async fn expect_request_matches_navigation() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let ctx = browser.new_context().await.unwrap();
    let page = ctx.new_page().await.unwrap();
    let url = common::http_server("<html><body><p>ok</p></body></html>").await;

    // Race the wait against the navigation that triggers the request.
    let (req, _) = tokio::join!(
        ctx.expect_request("127.0.0.1", Some(Duration::from_secs(5))),
        page.goto(&url, None),
    );

    let req = req.expect("expected a matching request");
    assert!(req.url().contains("127.0.0.1"));

    ctx.close().await.unwrap();
    browser.close().await.unwrap();
}

/// `expect_console` resolves with the console message whose text contains the
/// predicate.
#[tokio::test]
async fn expect_console_matches_log() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let ctx = browser.new_context().await.unwrap();
    let page = ctx.new_page().await.unwrap();
    // A console subscription needs a document; navigate to about:blank first.
    page.goto("about:blank", None).await.unwrap();

    // Race the wait against the script that logs the message.
    let (msg, _) = tokio::join!(
        ctx.expect_console("hello", Some(Duration::from_secs(5))),
        page.evaluate::<serde_json::Value>("console.log('hello world')"),
    );

    let msg = msg.expect("expected a matching console message");
    assert!(msg.text().contains("hello"));

    ctx.close().await.unwrap();
    browser.close().await.unwrap();
}

/// A predicate that never matches must time out and return `Error::Timeout`
/// (rather than hanging forever).
#[tokio::test]
async fn expect_page_times_out_when_no_match() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let ctx = browser.new_context().await.unwrap();

    // Register the wait first; nothing opens a page, so it times out.
    let res = ctx
        .expect_page(Some(Duration::from_millis(500)))
        .await;

    match res {
        Ok(_) => panic!("expected a timeout error, but a page matched"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("Timeout") || msg.contains("timeout"),
                "expected a Timeout error, got: {msg}"
            );
        }
    }

    ctx.close().await.unwrap();
    browser.close().await.unwrap();
}
