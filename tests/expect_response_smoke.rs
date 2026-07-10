//! Smoke tests for the `Page` event-waiter helpers `expect_response` /
//! `expect_request` (built on the one-shot handler pattern). Skips gracefully
//! if no browser is available.

mod common;

use std::time::Duration;

use playwright_cdp::Error;

#[tokio::test]
async fn expect_response_matches_navigation() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server("<html><body>x</body></html>").await;

    let (resp, _) = tokio::join!(
        page.expect_response("127.0.0.1", Some(Duration::from_secs(5))),
        page.goto(&url, None),
    );
    let resp = match resp {
        Ok(r) => r,
        Err(e) => panic!("expect_response failed: {e:?}"),
    };
    assert_eq!(resp.status(), 200);
    assert!(
        resp.url().contains("127.0.0.1"),
        "response url should contain predicate: {}",
        resp.url()
    );

    browser.close().await.unwrap();
}

#[tokio::test]
async fn expect_request_matches_navigation() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server("<html><body>x</body></html>").await;

    let (req, _) = tokio::join!(
        page.expect_request("127.0.0.1", Some(Duration::from_secs(5))),
        page.goto(&url, None),
    );
    let req = match req {
        Ok(r) => r,
        Err(e) => panic!("expect_request failed: {e:?}"),
    };
    assert!(
        req.url().contains("127.0.0.1"),
        "request url should contain predicate: {}",
        req.url()
    );

    browser.close().await.unwrap();
}

/// A predicate that never matches must time out and return `Error::Timeout`
/// rather than hanging.
#[tokio::test]
async fn expect_response_times_out_when_no_match() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    let res = page
        .expect_response(
            "definitely-not-a-real-substring",
            Some(Duration::from_millis(500)),
        )
        .await;

    match res {
        Ok(r) => panic!("expected a timeout, but got a response: status {}", r.status()),
        Err(Error::Timeout(_)) => {}
        Err(e) => panic!("expected Error::Timeout, got {e:?}"),
    }

    browser.close().await.unwrap();
}
